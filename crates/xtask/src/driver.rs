// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HOST-LANE DRIVER: which `cargo`-shaped program the gates spawn, and which
//! `rustc`-shaped program answers for the box's own triple.
//!
//! Until 2026-09-18 nine call sites in `gate.rs` and `perf.rs` spelled a bare
//! `Command::new("cargo")`, and one spelled a bare `Command::new("rustc")`. On a
//! machine provisioned the product's way (`aterm pkg install trust`) there is NO
//! rustup and NO stock `cargo`/`rustc` on PATH — measured on the owner's Mac that
//! day: `command -v cargo rustc rustup` prints nothing, `~/.cargo` and
//! `~/.rustup` do not exist, and the toolchain lives ONLY in the atpkg store
//! (`<prefix>/store/trust/<build>/bin/{targo,trustc,…}`, with
//! `<prefix>/bin/{targo,trustc}` as the shims on PATH; `aterm pkg which targo`
//! prints the resolution). There, `gate cells` printed `COULD NOT RUN — rustc
//! -vV did not report a host triple`, three unit tests panicked on the same
//! probe, and every `cargo run --release -p aterm-bench` perf lane could not
//! spawn. This module is the ONE resolution all of them share, so the host lane
//! cannot half-migrate: the store first, stock cargo as a documented fallback.
//!
//! THE CROSS LANES ARE NOT HERE. The x86_64/linux/win/wasm32 cells ride upstream
//! stable through rustup by design (rust-toolchain.toml's header lists them); a
//! cross cell's `cargo` spelling stays a rustup proxy with `RUSTUP_TOOLCHAIN`
//! set, because the Trust sysroot carries only the host std.
//!
//! TWO FACTS ABOUT TARGO, MEASURED 2026-09-18 against `targo 1.99.0-dev
//! (321aaeda7 2026-09-17)` from store build 9192, that decide the shape below:
//!
//!   * The lane flag is PER VERB. `targo check`/`targo test` refuse an implicit
//!     lane ("refuses to create an implicitly unverified artifact … `targo
//!     --unverified check`"); `targo metadata --no-deps …` runs with NO flag,
//!     and `targo --unverified metadata …` (flag before or after the verb) is
//!     refused: "`--unverified` is valid only for a Targo compilation command".
//!     So [`CargoDriver::command`] takes the verb and adds the flag only where
//!     targo accepts it.
//!   * The outer lane does NOT reach children. `targo run` prints "nested
//!     explicit-unverified Targo propagation is unavailable on this platform;
//!     … recursive `$CARGO` invocations must select their own lane", so a gate
//!     spawned by `targo --unverified run -p xtask` still has to name the lane
//!     on every child it starts. `$CARGO` names the driver; the lane is ours.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The `cargo`-shaped program the host lane spawns, resolved once per process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoDriver {
    /// The program to spawn — an absolute path, or the bare name `cargo` in the
    /// last-resort case.
    pub(crate) program: PathBuf,
    /// Did `<program> --version` answer as targo? Decides the lane flag.
    pub(crate) is_targo: bool,
    /// Which rung of the ladder answered — printed with the command so a gate
    /// log says which toolchain compiled, never leaving the reader to guess.
    pub(crate) source: &'static str,
    /// `aterm_verify`'s own diagnosis when the toolchain it settled on was
    /// REFUSED (a `targo` there that is not the pin) — the ladder skipped that
    /// directory and the gate prints this beside the driver it used instead,
    /// so a fall-through to a PATH targo or bare `cargo` is never silent.
    pub(crate) refused: Option<String>,
}

/// The verbs targo calls "compilation commands" — the ONLY ones that accept
/// (and, for `check`/`test`/`build`, REQUIRE) `--unverified`. Measured
/// 2026-09-18: `metadata` refuses the flag outright. A verb not in this list
/// gets no lane flag, which is right for every non-compiling verb and is loud
/// (targo's own refusal) rather than silent for a compiling verb this list has
/// not learned yet.
const LANE_VERBS: &[&str] = &["build", "check", "test", "run", "bench", "doc", "rustc"];

impl CargoDriver {
    /// The lane flag `verb` needs from this driver: `--unverified` when the
    /// driver is targo and the verb compiles, nothing otherwise (stock cargo has
    /// no such flag, and targo refuses it on `metadata`).
    pub(crate) fn lane_args(&self, verb: &str) -> &'static [&'static str] {
        if self.is_targo && LANE_VERBS.contains(&verb) {
            &["--unverified"]
        } else {
            &[]
        }
    }

    /// `<program> [--unverified] <verb>`, ready for the caller's own arguments.
    pub(crate) fn command(&self, verb: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.lane_args(verb)).arg(verb);
        cmd
    }

    /// The command line [`Self::command`] builds, for the `$ …` log line.
    pub(crate) fn display(&self, verb: &str, args: &[&str]) -> String {
        let mut parts: Vec<String> = vec![self.program.display().to_string()];
        parts.extend(self.lane_args(verb).iter().map(|s| (*s).to_string()));
        parts.push(verb.to_string());
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }

    /// The full argv after the program: `[--unverified] <verb> <args…>`.
    pub(crate) fn argv(&self, verb: &str, args: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = self
            .lane_args(verb)
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        out.push(verb.to_string());
        out.extend(args.iter().map(|s| (*s).to_string()));
        out
    }
}

/// Does `<program> --version` answer as targo? The same sniff
/// `crates/aterm-gui/src/lib.rs`'s `cargo_lane_args` uses: ask the binary
/// rather than trust its file name OR its directory. Measured 2026-09-18
/// against store build 9192: the stock-NAMED `cargo` that ships beside
/// `targo` in the SAME store directory answers `cargo 1.99.0-dev (321aaeda7
/// 2026-09-17)` — no `targo` in it — and REFUSES the lane flag (`cargo
/// --unverified metadata` → "unexpected argument '--unverified'"), while
/// `targo --version` there answers `targo 1.99.0-dev (…) (targo 0.1.0)`. So
/// being in the store proves nothing about the flag; only a program that
/// answers `targo` takes it. That is also what keeps rung 4 working through a
/// rustup `trust` link, whose shim `cargo` answers `cargo …` and would reject
/// the flag.
fn answers_as_targo(program: &Path) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("targo")
        })
}

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

/// The first `name` on `path_env` whose PHYSICAL location is under `prefix` —
/// the atpkg shim (`<prefix>/bin/targo`) resolves into `<prefix>/store/…`, so
/// a targo that is the product's is accepted and a stray one (Homebrew, a
/// `~/.local/bin` farm link) is not.
///
/// Both sides are compared PHYSICALLY: the prefix too, since a home under a
/// symlink (or a `/var` → `/private/var` scratch dir) would otherwise never
/// match its own store.
fn on_path_under(name: &str, path_env: &OsStr, prefix: &Path) -> Option<PathBuf> {
    let prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    std::env::split_paths(path_env)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(name))
        .filter(|p| is_executable_file(p))
        .find_map(|p| {
            let physical = std::fs::canonicalize(&p).ok()?;
            physical.starts_with(&prefix).then_some(p)
        })
}

/// THE LADDER, pure so a test can climb each rung. First hit wins:
///
/// 1. `env_cargo` — `$CARGO`, the driver that runs THIS process (cargo and
///    targo both set it for build scripts, tests and `run` children). Under
///    `targo --unverified run -p xtask` that is the store's targo; keeping it
///    keeps the outer lane's toolchain.
/// 2. `<pinned_stage2>/targo` — the toolchain `aterm_verify::Toolchain`
///    resolved (`$TRUST_STAGE2_BIN`, the promoted links, the atpkg store
///    `<prefix>/store/trust/current/bin`, the stage2 tree, then PATH) AND
///    accepted as the pin. `None` here means it REFUSED what it found (a
///    `targo` with no branded `trustc` beside it — see [`pinned_stage2_of`]),
///    and a refused directory is skipped, never adopted: the same fail-closed
///    branch `gate tippy`/`gate fmt` take, so `gate cells` cannot print GREEN
///    under a frontend the verify driver would not run. On a
///    product-provisioned box this rung IS the store.
/// 3. `targo` on `path_env` resolving physically under the atpkg `prefix` —
///    the shim `aterm pkg install trust` lays, on a box whose store the
///    mirror in step 2 could not place.
/// 4. bare `cargo` — the documented fallback for a rustup-provisioned box.
///    It resolves through rustup's shim to `rust-toolchain.toml`'s `trust`
///    link there, and to nothing at all on a Trust-only box, where the spawn
///    error is the gate's own line.
pub(crate) fn resolve_cargo_driver(
    env_cargo: Option<&OsStr>,
    pinned_stage2: Option<&Path>,
    path_env: &OsStr,
    prefix: &Path,
) -> (PathBuf, &'static str) {
    if let Some(c) = env_cargo.filter(|c| !c.is_empty()) {
        return (PathBuf::from(c), "$CARGO");
    }
    if let Some(store) = pinned_stage2
        .map(|d| d.join("targo"))
        .filter(|p| is_executable_file(p))
    {
        return (store, "the pinned toolchain (aterm pkg which targo)");
    }
    if let Some(p) = on_path_under("targo", path_env, prefix) {
        return (p, "targo on PATH under the atpkg prefix");
    }
    (
        PathBuf::from("cargo"),
        "bare `cargo` on PATH (rustup fallback)",
    )
}

/// The stage2 `bin` a discovered toolchain may be DRIVEN from: its directory
/// when `have_targo()` holds (an executable `targo` there, and the pin
/// accepted), `None` otherwise.
///
/// A discovered toolchain's `stage2_dir` is a REFUSED directory too — it is
/// what the operator pointed at, and the "no trustc here" probes answer no
/// about it by construction. That is the right shape for a probe and
/// the wrong one for a driver: until 2026-09-18 the ladder joined `targo` onto
/// that directory and adopted it on mere executability, so an impostor at
/// `$TRUST_STAGE2_BIN` (measured that day on the owner's Mac: a shell script
/// printing `targo 0.0.0-fake`, no `trustc` beside it) became `gate cells`'
/// compiler and was announced as "the pinned toolchain". This helper is the
/// seam that asks the toolchain's own verdict instead.
pub(crate) fn pinned_stage2_of(toolchain: &aterm_verify::Toolchain) -> Option<PathBuf> {
    toolchain.have_targo().then(|| toolchain.stage2_dir.clone())
}

/// [`pinned_stage2_of`] the live discovery, with the refusal text (when the
/// discovery refused something) for the gate to print.
fn pinned_stage2_bin() -> (Option<PathBuf>, Option<String>) {
    let toolchain = crate::gate::trust_toolchain();
    let refused = toolchain
        .refused
        .is_some()
        .then(|| toolchain.missing_targo_label());
    (pinned_stage2_of(&toolchain), refused)
}

/// The driver every host-lane gate spawns, resolved once and cached.
pub(crate) fn cargo_driver() -> &'static CargoDriver {
    static DRIVER: OnceLock<CargoDriver> = OnceLock::new();
    DRIVER.get_or_init(|| {
        let home = std::env::var_os("HOME").unwrap_or_default();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let prefix = aterm_verify::toolchain::atpkg_prefix(Path::new(&home), xdg.as_deref());
        let (pinned, refused) = pinned_stage2_bin();
        let (program, source) = resolve_cargo_driver(
            std::env::var_os("CARGO").as_deref(),
            pinned.as_deref(),
            &path,
            &prefix,
        );
        let is_targo = answers_as_targo(&program);
        CargoDriver {
            program,
            is_targo,
            source,
            refused,
        }
    })
}

/// Hand the resolved driver to the LIBRARIES this process calls, as `$CARGO`.
///
/// `aterm_forge::resolve` (the `cargo tree` behind `gate cells`' graph and all
/// of `gate forge`) spawns `$CARGO` when set and a bare `cargo` otherwise —
/// forge is a library and rightly keeps no ladder of its own. Under `targo
/// --unverified run -p xtask` targo has already set `$CARGO`; run the xtask
/// binary DIRECTLY on a Trust-only box and it is unset, so forge's bare `cargo`
/// fails to spawn and `gate cells` reports `cargo tree could not resolve cell`
/// with the driver announced two lines earlier (measured 2026-09-18, owner's
/// Mac, `env -u CARGO target/debug/xtask gate cells --cell mac-arm`). Measured
/// the same day: `targo tree …` runs with no lane flag (and refuses one, like
/// `metadata`), so the store's targo is a correct `$CARGO` for forge verbatim.
///
/// Only ever SETS an unset variable — a driver the parent chose is never
/// overridden — and does so at the top of a gate verb, before any thread
/// exists, which is the single-threaded-startup justification the mutation
/// needs.
///
/// The write goes through [`aterm_log::env::set`], the workspace's ONE
/// lock-scoped env helper, and not through a bare `std::env::set_var`. Two
/// reasons, and the first is the lint: the Trust toolchain's `env_mutation`
/// fires on a raw mutation wherever it sits, so under the gate's
/// `tippy --workspace --all-targets -- -D warnings` a raw call here is a hard
/// red (measured on the owner's Intel Mac 2026-09-19: the lint lane failed
/// exit 101 on this one line, the lane's first red since 2026-09-17). The
/// second is the reason the lint exists: a lock only serializes the mutators
/// that SHARE it, so the useful lock is the one every crate in the binary
/// already links, which is `aterm-log`'s — a private lock in xtask would be
/// theatre, as that module's own docs say. Being the one lock, it also bounds
/// this write against a `read` taken through the same module anywhere else in
/// the process.
///
/// What the helper does NOT buy is safety against a `getenv` in a C library on
/// another thread, so the startup ordering above is still load-bearing and is
/// still the real argument.
pub(crate) fn export_driver_as_cargo() -> &'static CargoDriver {
    let driver = cargo_driver();
    if std::env::var_os("CARGO").is_none_or(|c| c.is_empty()) {
        aterm_log::env::set("CARGO", &driver.program);
    }
    driver
}

/// The remedy line every "no driver" refusal carries — the product's way first,
/// rustup second.
pub(crate) const DRIVER_REMEDY: &str = "install the toolchain the product's way (`aterm pkg install \
                                        trust`; `aterm pkg which targo` shows the resolution), or \
                                        provision rustup with the `trust` link";

/// `host: <triple>` out of a `rustc -vV` / `trustc -vV` listing.
pub(crate) fn parse_host_triple(vv: &str) -> Option<String> {
    vv.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// THE LADDER for the box's own triple, pure so a test can inject each rung.
/// Every entry is asked `-vV` in order and the first that answers wins:
///
/// 1. `env_rustc` — `$RUSTC`, which cargo and targo both set for build
///    scripts, tests and `run` children (under targo it is `trustc`).
/// 2. `$CARGO`'s siblings `rustc`, then `trustc` — the compiler that ships
///    beside the driver running this process (the store's `rustc` is a hard
///    link of `trustc`; a rustup proxy's sibling `rustc` is the shim that
///    honours the repo pin).
/// 3. `<pinned_stage2>/trustc` — the toolchain the store resolution settled
///    on AND accepted as the pin ([`pinned_stage2_of`]); on a
///    product-provisioned box that is `<prefix>/store/trust/current/bin/trustc`,
///    the path `aterm pkg which trustc` prints. A REFUSED directory is `None`
///    and contributes no rung: its `trustc` (if it even has one) is not the
///    pinned compiler, and this ladder must not call it that.
/// 4. bare `rustc` on PATH — the rustup fallback, last, and the ONLY rung the
///    pre-2026-09-18 probe had.
pub(crate) fn host_triple_candidates(
    env_rustc: Option<&OsStr>,
    env_cargo: Option<&OsStr>,
    pinned_stage2: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(r) = env_rustc.filter(|r| !r.is_empty()) {
        out.push(PathBuf::from(r));
    }
    if let Some(dir) = env_cargo
        .filter(|c| !c.is_empty())
        .and_then(|c| Path::new(c).parent().map(Path::to_path_buf))
        .filter(|d| !d.as_os_str().is_empty())
    {
        out.push(dir.join("rustc"));
        out.push(dir.join("trustc"));
    }
    if let Some(dir) = pinned_stage2 {
        out.push(dir.join("trustc"));
    }
    out.push(PathBuf::from("rustc"));
    out
}

/// This compiler's own host triple, from the first candidate on
/// [`host_triple_candidates`] that answers `-vV`. `None` means no compiler on
/// the ladder answered — the caller names [`DRIVER_REMEDY`].
pub(crate) fn rustc_host_triple() -> Option<String> {
    let candidates = host_triple_candidates(
        std::env::var_os("RUSTC").as_deref(),
        std::env::var_os("CARGO").as_deref(),
        pinned_stage2_bin().0.as_deref(),
    );
    candidates.iter().find_map(|c| host_triple_of(c))
}

/// `host: …` from `<compiler> -vV`, or `None` when it cannot run or does not
/// report one.
pub(crate) fn host_triple_of(compiler: &Path) -> Option<String> {
    let out = Command::new(compiler).arg("-vV").output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_host_triple(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xtask-driver-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn touch_exe(p: &Path) {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, "#!/bin/sh\nexit 0\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }

    fn empty_stage2() -> PathBuf {
        scratch("nostage2").join("absent/bin")
    }

    // -------------------------------------------------------------------
    // THE HOST-TRIPLE LADDER, one rung at a time
    // -------------------------------------------------------------------

    #[test]
    fn env_rustc_is_the_first_rung() {
        let stage2 = Path::new("/store/trust/current/bin");
        let c = host_triple_candidates(
            Some(OsStr::new("/env/trustc")),
            Some(OsStr::new("/env/targo")),
            Some(stage2),
        );
        assert_eq!(
            c,
            vec![
                PathBuf::from("/env/trustc"),
                PathBuf::from("/env/rustc"),
                PathBuf::from("/env/trustc"),
                PathBuf::from("/store/trust/current/bin/trustc"),
                PathBuf::from("rustc"),
            ]
        );
    }

    #[test]
    fn cargos_siblings_come_second_rustc_before_trustc() {
        let c = host_triple_candidates(
            None,
            Some(OsStr::new("/p/store/trust/9192/bin/targo")),
            Some(Path::new("/p/store/trust/current/bin")),
        );
        assert_eq!(c[0], PathBuf::from("/p/store/trust/9192/bin/rustc"));
        assert_eq!(c[1], PathBuf::from("/p/store/trust/9192/bin/trustc"));
        assert_eq!(c[2], PathBuf::from("/p/store/trust/current/bin/trustc"));
        assert_eq!(c[3], PathBuf::from("rustc"));
    }

    #[test]
    fn the_store_is_third_and_bare_rustc_is_last() {
        let c = host_triple_candidates(None, None, Some(Path::new("/p/store/trust/current/bin")));
        assert_eq!(
            c,
            vec![
                PathBuf::from("/p/store/trust/current/bin/trustc"),
                PathBuf::from("rustc"),
            ]
        );
    }

    #[test]
    fn empty_env_values_are_unset_not_candidates() {
        let c = host_triple_candidates(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(Path::new("/s/bin")),
        );
        assert_eq!(
            c,
            vec![PathBuf::from("/s/bin/trustc"), PathBuf::from("rustc")]
        );
    }

    #[test]
    fn a_bare_cargo_name_has_no_sibling_dir() {
        // `$CARGO=cargo` (no slash) has an empty parent; asking for `/rustc`
        // beside it would be a candidate at the filesystem root.
        let c = host_triple_candidates(None, Some(OsStr::new("cargo")), Some(Path::new("/s/bin")));
        assert_eq!(
            c,
            vec![PathBuf::from("/s/bin/trustc"), PathBuf::from("rustc")]
        );
    }

    /// The `-vV` shape both compilers print — this is the store's `trustc -vV`
    /// verbatim (2026-09-18, build 9192), which carries an extra `trust:` row
    /// a stock rustc lacks.
    #[test]
    fn parse_reads_the_host_row_from_trustc_and_rustc_alike() {
        let trustc = "rustc 1.99.0-dev (321aaeda7 2026-09-17) (trustc 0.1.0)\nbinary: trustc\n\
                      commit-hash: 321aaeda75478038420d97830f14724c33c8dc39\n\
                      commit-date: 2026-09-17\nhost: aarch64-apple-darwin\nrelease: 1.99.0-dev\n\
                      trust: 0.1.0\nLLVM version: 22.1.2\n";
        assert_eq!(
            parse_host_triple(trustc).as_deref(),
            Some("aarch64-apple-darwin")
        );
        let rustc =
            "rustc 1.90.0 (1159e78c4 2025-09-14)\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\n";
        assert_eq!(
            parse_host_triple(rustc).as_deref(),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(parse_host_triple("binary: rustc\n"), None);
        assert_eq!(parse_host_triple("host: \n"), None);
    }

    /// A rung that cannot run is skipped, not fatal: the ladder's whole point
    /// is that the bare `rustc` at the bottom is absent on a Trust-only box.
    #[test]
    fn a_missing_compiler_answers_none() {
        assert_eq!(host_triple_of(Path::new("/nonexistent/xtask/trustc")), None);
    }

    /// The live ladder answers on every box this repo builds on — the store's
    /// `trustc` on a product-provisioned Mac (no `rustc` on PATH at all), the
    /// rustup shim elsewhere — and it agrees with whichever compiler the
    /// driver running this test set as `$RUSTC`.
    #[test]
    fn the_live_ladder_answers_and_agrees_with_the_test_drivers_compiler() {
        let live = rustc_host_triple().expect("some compiler on the ladder reports a host triple");
        if let Some(r) = std::env::var_os("RUSTC").filter(|r| !r.is_empty()) {
            assert_eq!(
                host_triple_of(Path::new(&r)).as_deref(),
                Some(live.as_str())
            );
        }
    }

    // -------------------------------------------------------------------
    // THE CARGO-DRIVER LADDER
    // -------------------------------------------------------------------

    #[test]
    fn env_cargo_wins_over_everything() {
        let dir = scratch("envcargo");
        let stage2 = dir.join("store/trust/current/bin");
        touch_exe(&stage2.join("targo"));
        let (p, src) = resolve_cargo_driver(
            Some(OsStr::new("/driver/targo")),
            Some(&stage2),
            &OsString::new(),
            &dir,
        );
        assert_eq!(p, PathBuf::from("/driver/targo"));
        assert_eq!(src, "$CARGO");
    }

    #[test]
    fn the_stores_targo_is_second() {
        let dir = scratch("store");
        let stage2 = dir.join("store/trust/current/bin");
        touch_exe(&stage2.join("targo"));
        let (p, src) = resolve_cargo_driver(None, Some(&stage2), &OsString::new(), &dir);
        assert_eq!(p, stage2.join("targo"));
        assert!(src.contains("aterm pkg which targo"), "{src}");
    }

    #[test]
    fn a_path_targo_counts_only_when_it_resolves_under_the_prefix() {
        let prefix = scratch("prefix");
        let elsewhere = scratch("elsewhere");
        // A shim in <prefix>/bin that links into <prefix>/store — atpkg's layout.
        let real = prefix.join("store/trust/9192/bin/targo");
        touch_exe(&real);
        let shim_dir = prefix.join("bin");
        std::fs::create_dir_all(&shim_dir).expect("mkdir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, shim_dir.join("targo")).expect("symlink");
        #[cfg(not(unix))]
        touch_exe(&shim_dir.join("targo"));
        // A stray targo earlier on PATH, outside the prefix: skipped.
        let stray_dir = elsewhere.join("bin");
        touch_exe(&stray_dir.join("targo"));
        let path = std::env::join_paths([stray_dir.clone(), shim_dir.clone()]).expect("PATH");
        let (p, src) = resolve_cargo_driver(None, Some(&empty_stage2()), &path, &prefix);
        assert_eq!(p, shim_dir.join("targo"));
        assert!(src.contains("PATH"), "{src}");
        // With only the stray one, the ladder falls through to bare cargo.
        let path = std::env::join_paths([stray_dir]).expect("PATH");
        let (p, src) = resolve_cargo_driver(None, Some(&empty_stage2()), &path, &prefix);
        assert_eq!(p, PathBuf::from("cargo"));
        assert!(src.contains("rustup fallback"), "{src}");
    }

    #[test]
    fn bare_cargo_is_the_last_rung() {
        let (p, src) = resolve_cargo_driver(
            Some(OsStr::new("")),
            Some(&empty_stage2()),
            &OsString::new(),
            Path::new("/nonexistent/prefix"),
        );
        assert_eq!(p, PathBuf::from("cargo"));
        assert_eq!(src, "bare `cargo` on PATH (rustup fallback)");
    }

    /// A toolchain aterm-verify REFUSED (a `targo` that is not the pin) yields
    /// no stage2 for either ladder: with an executable `targo` right there,
    /// `refused` alone flips the answer to `None`, and the driver ladder falls
    /// through to bare `cargo` rather than adopting the impostor. Measured
    /// 2026-09-18 before this seam existed: `TRUST_STAGE2_BIN=<impostor>/bin
    /// xtask gate cells --cell mac-arm` announced the impostor as "the pinned
    /// toolchain (aterm pkg which targo)" and ran it.
    #[test]
    fn a_refused_stage2_is_neither_the_driver_nor_the_compiler() {
        let dir = scratch("refused");
        let stage2 = dir.join("impostor/bin");
        touch_exe(&stage2.join("targo"));
        touch_exe(&stage2.join("trustc"));
        let accepted = aterm_verify::Toolchain {
            stage2_dir: stage2.clone(),
            targo: stage2.join("targo"),
            trustdoc: stage2.join("trustdoc"),
            tippy: None,
            refused: None,
            store_bin: None,
        };
        assert!(accepted.have_targo());
        assert_eq!(
            pinned_stage2_of(&accepted).as_deref(),
            Some(stage2.as_path())
        );
        let refused = aterm_verify::Toolchain {
            refused: Some(stage2.clone()),
            ..accepted
        };
        assert!(!refused.have_targo());
        let pinned = pinned_stage2_of(&refused);
        assert_eq!(pinned, None, "a refused directory is not a pinned stage2");
        // The driver ladder: rung 2 gone, nothing on PATH → bare cargo.
        let (p, src) = resolve_cargo_driver(
            None,
            pinned.as_deref(),
            &OsString::new(),
            Path::new("/nonexistent/prefix"),
        );
        assert_eq!(p, PathBuf::from("cargo"));
        assert!(src.contains("rustup fallback"), "{src}");
        // The compiler ladder: the impostor's `trustc` is not a candidate.
        let c = host_triple_candidates(None, None, pinned.as_deref());
        assert_eq!(c, vec![PathBuf::from("rustc")]);
        assert!(
            !c.iter().any(|p| p.starts_with(&stage2)),
            "the refused directory contributed a rung: {c:?}"
        );
    }

    /// The same, end to end through aterm-verify's own discovery: an explicit
    /// `$TRUST_STAGE2_BIN` holding an executable `targo` and NO `trustc` is
    /// exactly what the pin check refuses (no branded rustc beside it), and
    /// the live seam turns that into `None`.
    #[test]
    fn discovery_of_an_impostor_stage2_is_refused_and_not_driven() {
        let dir = scratch("impostor-discover");
        let stage2 = dir.join("bin");
        touch_exe(&stage2.join("targo"));
        let toolchain = aterm_verify::Toolchain::discover_with_store(
            Some(&stage2),
            &dir.join("home"),
            None,
            &OsString::new(),
            Some("trust"),
        );
        assert!(
            toolchain.refused.is_some(),
            "an explicit stage2 with a targo and no trustc must be refused: {toolchain:?}"
        );
        assert!(!toolchain.have_targo());
        assert_eq!(pinned_stage2_of(&toolchain), None);
        assert!(
            toolchain
                .missing_targo_label()
                .contains("NOT the toolchain"),
            "{}",
            toolchain.missing_targo_label()
        );
    }

    // -------------------------------------------------------------------
    // THE LANE FLAG, per verb
    // -------------------------------------------------------------------

    #[test]
    fn targo_gets_the_lane_flag_on_compiling_verbs_only() {
        let d = CargoDriver {
            program: PathBuf::from("/s/targo"),
            is_targo: true,
            source: "test",
            refused: None,
        };
        for verb in ["build", "check", "test", "run", "bench", "doc", "rustc"] {
            assert_eq!(d.lane_args(verb), &["--unverified"], "{verb}");
        }
        // Measured 2026-09-18: `targo --unverified metadata` is refused.
        assert_eq!(d.lane_args("metadata"), &[] as &[&str]);
        assert_eq!(
            d.argv("metadata", &["--no-deps", "--format-version", "1"]),
            vec!["metadata", "--no-deps", "--format-version", "1"]
        );
        assert_eq!(
            d.argv("check", &["--locked"]),
            vec!["--unverified", "check", "--locked"]
        );
        assert_eq!(
            d.display("run", &["--release", "-p", "aterm-bench"]),
            "/s/targo --unverified run --release -p aterm-bench"
        );
    }

    #[test]
    fn stock_cargo_gets_no_lane_flag() {
        let d = CargoDriver {
            program: PathBuf::from("cargo"),
            is_targo: false,
            source: "test",
            refused: None,
        };
        assert_eq!(d.lane_args("check"), &[] as &[&str]);
        assert_eq!(d.argv("test", &["-p", "x"]), vec!["test", "-p", "x"]);
        assert_eq!(d.display("metadata", &[]), "cargo metadata");
    }

    /// The live driver can be spawned, and when the test itself runs under
    /// targo (`$CARGO` answers as targo) the lane flag is on — the property
    /// that keeps a gate spawned by `targo --unverified run -p xtask` from
    /// tripping targo's implicit-lane refusal on its first child.
    #[test]
    fn the_live_driver_runs_and_matches_the_process_driver() {
        let d = cargo_driver();
        let out = Command::new(&d.program)
            .arg("--version")
            .output()
            .unwrap_or_else(|e| panic!("{} --version ({}): {e}", d.program.display(), d.source));
        assert!(
            out.status.success(),
            "{} --version failed",
            d.program.display()
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(d.is_targo, text.contains("targo"), "{text}");
        if let Some(c) = std::env::var_os("CARGO").filter(|c| !c.is_empty()) {
            assert_eq!(d.program, PathBuf::from(c));
            assert_eq!(d.source, "$CARGO");
        }
    }
}
