// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `targo` the perf lanes build with: the run's own toolchain
//! ([`crate::run_toolchain`]), refused rather than replaced when it is not the
//! pin, so no lane measures a build from another compiler under the pin's name.

use std::path::PathBuf;
use std::sync::OnceLock;

use aterm_uds::spawnfd::{Command, command};

/// The pinned toolchain's `targo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoDriver {
    pub(crate) program: PathBuf,
}

/// targo's compiling verbs, the only ones that take `--unverified` (and the
/// only ones that require a lane); `metadata` and `tree` refuse it.
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
        let mut cmd = command(&self.program);
        cmd.args(Self::lead(verb, None));
        cmd
    }

    /// [`Self::command`] for a build of THIS workspace, led by its build-dir
    /// `--config` ([`workspace_build_dir`]) when the verb compiles, so a perf
    /// build shares a shell build's cache and keeps its intermediates out of
    /// Spotlight.
    pub(crate) fn workspace_command(&self, verb: &str) -> Command {
        let mut cmd = command(&self.program);
        cmd.args(Self::lead(verb, workspace_build_dir()));
        cmd
    }

    /// What goes before the caller's arguments: `--config <build_dir>` for a
    /// compiling verb given one, the lane flag, the verb.
    fn lead(verb: &str, build_dir: Option<&str>) -> Vec<String> {
        let mut lead = Vec::new();
        if let Some(value) = build_dir.filter(|_| LANE_VERBS.contains(&verb)) {
            lead.extend(["--config".to_string(), value.to_string()]);
        }
        lead.extend(Self::lane_args(verb).iter().map(|s| (*s).to_string()));
        lead.push(verb.to_string());
        lead
    }

    /// The command line [`Self::workspace_command`] builds, for the `$ …` log line.
    pub(crate) fn display_workspace(&self, verb: &str, args: &[&str]) -> String {
        self.display_led(verb, workspace_build_dir(), args)
    }

    /// The command line led by `build_dir` ([`Self::lead`]).
    fn display_led(&self, verb: &str, build_dir: Option<&str>, args: &[&str]) -> String {
        let mut parts: Vec<String> = vec![self.program.display().to_string()];
        parts.extend(Self::lead(verb, build_dir));
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }
}

/// The build-dir `--config` every perf build of this workspace leads with,
/// resolved once: plain `targo`'s cache where atpkg's shim would hand one
/// ([`crate::main_build_dir`]), else `<target>/build.noindex` under the target
/// directory `targo metadata` names ([`crate::fallback_build_dir`]), so the
/// intermediates stay out of Spotlight and the binaries still land in
/// `<target>/<profile>/`. `None` where the caller chose a build directory.
pub(crate) fn workspace_build_dir() -> Option<&'static str> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let env = crate::EnvSnapshot::capture();
        let root = super::workspace_root();
        crate::main_build_dir(&env, &root).or_else(|| {
            let target = target_dir().ok()?;
            crate::fallback_build_dir(&env, &root, &target)
        })
    })
    .as_deref()
}

/// The target directory: `targo`'s own answer through `metadata`, since a
/// caller's `CARGO_TARGET_DIR` (or the reroute's, or a cargo config) moves it.
pub(crate) fn target_dir() -> Result<PathBuf, String> {
    let driver = cargo_driver()?;
    let out = driver
        .command("metadata")
        .args(["--no-deps", "--format-version", "1"])
        .current_dir(super::workspace_root())
        .output()
        .map_err(|e| format!("could not run targo metadata: {e}"))?;
    let meta: aterm_json::Value = aterm_json::from_slice(&out.stdout)
        .map_err(|e| format!("targo metadata did not answer JSON: {e}"))?;
    meta.get("target_directory")
        .and_then(aterm_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "targo metadata names no target_directory".to_string())
}

/// The driver a discovered toolchain may be driven by, or why not.
pub(crate) fn driver_of(toolchain: &crate::Toolchain) -> Result<CargoDriver, String> {
    if toolchain.have_targo() {
        Ok(CargoDriver {
            program: toolchain.targo.clone(),
        })
    } else {
        Err(toolchain.missing_targo_label())
    }
}

/// The driver every perf lane spawns, resolved once per process.
pub(crate) fn cargo_driver() -> Result<&'static CargoDriver, &'static str> {
    static DRIVER: OnceLock<Result<CargoDriver, String>> = OnceLock::new();
    DRIVER
        .get_or_init(|| {
            let root = super::workspace_root();
            driver_of(&crate::run_toolchain(&crate::EnvSnapshot::capture(), &root))
        })
        .as_ref()
        .map_err(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An accepted toolchain is driven by its own `targo`; a refused one (a
    /// `targo` with no branded `trustc` beside it) drives nothing.
    #[test]
    fn a_refused_stage2_is_not_driven() {
        let stage2 = PathBuf::from("/s/bin");
        let refused = crate::Toolchain {
            stage2_dir: stage2.clone(),
            targo: stage2.join("targo"),
            trustdoc: stage2.join("trustdoc"),
            tippy: None,
            trustfmt: None,
            borrowed: Vec::new(),
            refused: Some(stage2.clone()),
            store_bin: None,
            demoted: None,
        };
        let why = driver_of(&refused).expect_err("a refused directory drives nothing");
        assert!(why.contains("NOT the toolchain"), "{why}");
    }

    #[test]
    fn the_lane_flag_goes_on_compiling_verbs_only() {
        let d = CargoDriver {
            program: PathBuf::from("/s/targo"),
        };
        for verb in LANE_VERBS {
            assert_eq!(CargoDriver::lane_args(verb), &["--unverified"], "{verb}");
        }
        assert_eq!(
            d.display_led("metadata", None, &["--no-deps"]),
            "/s/targo metadata --no-deps"
        );
        assert_eq!(
            d.display_led("run", None, &["--release", "-p", "aterm-bench"]),
            "/s/targo --unverified run --release -p aterm-bench"
        );
        // A workspace build leads with its build dir; a verb that compiles nothing
        // never does.
        let dir = Some("build.build-dir='/w/target/build.noindex'");
        assert_eq!(
            d.display_led("build", dir, &["--release"]),
            "/s/targo --config build.build-dir='/w/target/build.noindex' --unverified build --release"
        );
        assert_eq!(d.display_led("metadata", dir, &[]), "/s/targo metadata");
    }
}
