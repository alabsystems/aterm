// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HOST-LANE DRIVER: the `targo` the host-lane gates spawn and the `trustc`
//! that answers for the box's own triple — both from THE toolchain
//! ([`crate::gate::trust_toolchain`], i.e. `aterm_verify::Toolchain::discover`,
//! the resolution `tools/verify.sh`'s driver runs), and from nowhere else.
//!
//! Discovery reads `$TRUST_STAGE2_BIN` first (the gate hands every child the
//! directory its header names) and REFUSES a candidate that is not the pin: a
//! `targo` with no branded `trustc` beside it. A refused or absent toolchain is
//! a refusal here too, never a fall-through to some other `cargo` — on
//! 2026-09-18 an impostor at `$TRUST_STAGE2_BIN` (a script printing `targo
//! 0.0.0-fake`) became `gate cells`' compiler and was announced as the pinned
//! toolchain, and a `$CARGO`, PATH or bare-`cargo` rung is the same hole with a
//! different entrance.
//!
//! THE CROSS LANES ARE NOT HERE. The foreign cells ride rustup's `stable` (a
//! STOCK EXCEPTION: the Trust sysroot carries only the host std, E0463 for
//! every cross triple; rust-toolchain.toml lists them), driven by that
//! toolchain's own `cargo` and `rustc` (`gate::RustupToolchain`).
//!
//! Two facts about targo decide the shape below (measured 2026-09-18): the lane
//! flag is PER VERB — `check`/`test`/`build`/`run` refuse an implicit lane, while
//! `metadata` and `tree` refuse `--unverified` outright — and the outer lane does
//! not reach children, so a gate started by `targo --unverified run -p xtask`
//! names the lane on every child it spawns.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The pinned toolchain's `targo`, and the `trustc` beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoDriver {
    pub(crate) program: PathBuf,
    pub(crate) trustc: PathBuf,
}

/// The verbs targo calls compilation commands — the only ones that take (and,
/// for `check`/`test`/`build`, require) `--unverified`. A verb not listed gets
/// no flag, which is right for every non-compiling verb and loud (targo's own
/// refusal) for a compiling one this list has not learned.
const LANE_VERBS: &[&str] = &["build", "check", "test", "run", "bench", "doc", "rustc"];

impl CargoDriver {
    /// `--unverified` for a compiling verb, nothing otherwise.
    pub(crate) fn lane_args(verb: &str) -> &'static [&'static str] {
        if LANE_VERBS.contains(&verb) {
            &["--unverified"]
        } else {
            &[]
        }
    }

    /// `<targo> [--unverified] <verb>`, ready for the caller's own arguments.
    pub(crate) fn command(&self, verb: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(Self::lane_args(verb)).arg(verb);
        cmd
    }

    /// The command line [`Self::command`] builds, for the `$ …` log line.
    pub(crate) fn display(&self, verb: &str, args: &[&str]) -> String {
        let mut parts: Vec<String> = vec![self.program.display().to_string()];
        parts.extend(Self::lane_args(verb).iter().map(|s| (*s).to_string()));
        parts.push(verb.to_string());
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }
}

/// The driver a discovered toolchain may be driven by, or the refusal to print:
/// `have_targo()` holds only for an executable `targo` in a directory the pin
/// check accepted.
pub(crate) fn driver_of(toolchain: &aterm_verify::Toolchain) -> Result<CargoDriver, String> {
    if toolchain.have_targo() {
        Ok(CargoDriver {
            program: toolchain.targo.clone(),
            trustc: toolchain.stage2_dir.join("trustc"),
        })
    } else {
        Err(toolchain.missing_targo_label())
    }
}

/// The driver every host-lane gate spawns, resolved once per process.
pub(crate) fn cargo_driver() -> Result<&'static CargoDriver, &'static str> {
    static DRIVER: OnceLock<Result<CargoDriver, String>> = OnceLock::new();
    DRIVER
        .get_or_init(|| driver_of(&crate::gate::trust_toolchain()))
        .as_ref()
        .map_err(String::as_str)
}

/// Hand the driver to the LIBRARIES this process calls, as `$CARGO`:
/// `aterm_forge::resolve` (the `cargo tree` behind `gate cells`' graphs and all
/// of `gate forge`) spawns `$CARGO` when set and a bare `targo` otherwise. Only
/// ever SETS an unset variable, at the top of a gate verb before any thread
/// exists — the single-threaded-startup ordering is the real argument; the
/// write goes through [`aterm_log::env::set`], the binary's one env lock, which
/// the toolchain's `env_mutation` lint also asks for.
pub(crate) fn export_driver_as_cargo() -> Result<&'static CargoDriver, &'static str> {
    let driver = cargo_driver()?;
    if std::env::var_os("CARGO").is_none_or(|c| c.is_empty()) {
        aterm_log::env::set("CARGO", &driver.program);
    }
    Ok(driver)
}

/// `host: <triple>` out of a `trustc -vV` / `rustc -vV` listing.
pub(crate) fn parse_host_triple(vv: &str) -> Option<String> {
    vv.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// This box's own triple, as the pinned `trustc` reports it. `None` when there
/// is no pinned toolchain or its compiler does not answer.
pub(crate) fn rustc_host_triple() -> Option<String> {
    host_triple_of(&cargo_driver().ok()?.trustc)
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

    /// The `-vV` shape both compilers print — the store's `trustc -vV` verbatim
    /// (2026-09-18, build 9192), with the `trust:` row a stock rustc lacks.
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

    #[test]
    fn a_missing_compiler_answers_none() {
        assert_eq!(host_triple_of(Path::new("/nonexistent/xtask/trustc")), None);
    }

    /// An ACCEPTED toolchain is driven by its own `targo` and answers through
    /// the `trustc` beside it; a REFUSED one (a `targo` that is not the pin) is
    /// neither the driver nor the compiler, even with both files right there.
    /// Measured 2026-09-18 before this seam existed:
    /// `TRUST_STAGE2_BIN=<impostor>/bin xtask gate cells --cell mac-arm`
    /// announced the impostor as "the pinned toolchain" and ran it.
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
            demoted: None,
        };
        assert_eq!(
            driver_of(&accepted),
            Ok(CargoDriver {
                program: stage2.join("targo"),
                trustc: stage2.join("trustc"),
            })
        );
        let refused = aterm_verify::Toolchain {
            refused: Some(stage2.clone()),
            ..accepted
        };
        let why = driver_of(&refused).expect_err("a refused directory drives nothing");
        assert!(why.contains("NOT the toolchain"), "{why}");
    }

    /// The same, end to end through aterm-verify's own discovery: an explicit
    /// `$TRUST_STAGE2_BIN` holding an executable `targo` and NO `trustc` is what
    /// the pin check refuses, and the refusal is the answer — no rung below it.
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
        let why = driver_of(&toolchain).expect_err("an impostor is not driven");
        assert!(why.contains("NOT the toolchain"), "{why}");
    }

    #[test]
    fn the_lane_flag_goes_on_compiling_verbs_only() {
        let d = CargoDriver {
            program: PathBuf::from("/s/targo"),
            trustc: PathBuf::from("/s/trustc"),
        };
        for verb in ["build", "check", "test", "run", "bench", "doc", "rustc"] {
            assert_eq!(CargoDriver::lane_args(verb), &["--unverified"], "{verb}");
        }
        // Measured 2026-09-18: `targo --unverified metadata` is refused.
        assert_eq!(CargoDriver::lane_args("metadata"), &[] as &[&str]);
        assert_eq!(
            d.display("metadata", &["--no-deps", "--format-version", "1"]),
            "/s/targo metadata --no-deps --format-version 1"
        );
        assert_eq!(
            d.display("run", &["--release", "-p", "aterm-bench"]),
            "/s/targo --unverified run --release -p aterm-bench"
        );
    }

    /// The live driver is a targo that runs, and its `trustc` reports the same
    /// host triple as the compiler the test's own driver set as `$RUSTC`.
    #[test]
    fn the_live_driver_is_a_running_targo_and_agrees_with_the_test_drivers_compiler() {
        let d = cargo_driver().unwrap_or_else(|why| panic!("no pinned toolchain: {why}"));
        let out = Command::new(&d.program)
            .arg("--version")
            .output()
            .unwrap_or_else(|e| panic!("{} --version: {e}", d.program.display()));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("targo"), "{text}");
        let live = rustc_host_triple().expect("the pinned trustc reports a host triple");
        if let Some(r) = std::env::var_os("RUSTC").filter(|r| !r.is_empty()) {
            assert_eq!(
                host_triple_of(Path::new(&r)).as_deref(),
                Some(live.as_str())
            );
        }
    }
}
