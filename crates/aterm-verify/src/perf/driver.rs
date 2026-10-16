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
            d.display("metadata", &["--no-deps"]),
            "/s/targo metadata --no-deps"
        );
        assert_eq!(
            d.display("run", &["--release", "-p", "aterm-bench"]),
            "/s/targo --unverified run --release -p aterm-bench"
        );
    }
}
