// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pinned `targo` command builder for performance lanes.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::exec::check_command;
use aterm_uds::spawnfd::Command;

/// The pinned toolchain's `targo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoDriver {
    pub(crate) program: PathBuf,
}

const LANE_VERBS: &[&str] = &["build", "check", "test", "run", "bench", "doc", "rustc"];

/// Keep an outer `targo run`'s package metadata out of nested build fingerprints.
fn clear_run_env(command: &mut Command) {
    let explicit: Vec<_> = command.get_envs().map(|(key, _)| key.to_owned()).collect();
    for name in std::env::vars_os().map(|(key, _)| key).chain(explicit) {
        if name
            .to_str()
            .is_some_and(aterm_toolchain::build_cache::said_by_cargo_run)
        {
            command.env_remove(name);
        }
    }
}

impl CargoDriver {
    /// The pinned Cargo process with launcher metadata removed, ready for its arguments.
    pub(super) fn base_command(&self) -> Command {
        let mut command = check_command(&self.program);
        clear_run_env(&mut command);
        command
    }

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
        let mut cmd = self.base_command();
        cmd.args(Self::lead(verb, None));
        cmd
    }

    /// [`Self::command`] configured with [`workspace_build_dir`] for compiling verbs.
    pub(crate) fn workspace_command(&self, verb: &str) -> Command {
        let mut cmd = self.base_command();
        cmd.args(Self::lead(verb, workspace_build_dir()));
        cmd
    }

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

    fn display_led(&self, verb: &str, build_dir: Option<&str>, args: &[&str]) -> String {
        let mut parts: Vec<String> = vec![self.program.display().to_string()];
        parts.extend(Self::lead(verb, build_dir));
        parts.extend(args.iter().map(|s| (*s).to_string()));
        parts.join(" ")
    }
}

/// Resolves the `--config` build-dir argument for performance builds in this workspace.
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

/// Queries the workspace target directory from `targo metadata`.
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

    fn write_launcher_fixture(root: &std::path::Path) {
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"launcher_reuse_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 7 }\n").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            r#"fn main() {
    println!("{}|{}", std::env::var("CARGO_MANIFEST_DIR").unwrap(), std::env::var("OUT_DIR").unwrap());
}
"#,
        )
        .unwrap();
        // Intentional compiled-in metadata in this disposable build script reproduces
        // ring's environment fingerprint. Shipped code reads source paths at runtime.
        std::fs::write(
            root.join("build.rs"),
            r#"const _: &str = env!("CARGO_MANIFEST_DIR");
const _: Option<&str> = option_env!("OUT_DIR");
fn main() { println!("cargo:rerun-if-changed=build.rs"); }
"#,
        )
        .unwrap();
    }

    #[test]
    fn nested_build_reuses_artifacts_across_launcher_metadata() {
        const TEST: &str =
            "perf::driver::tests::nested_build_reuses_artifacts_across_launcher_metadata";
        const ROOT: &str = "ATERM_VERIFY_LAUNCHER_FIXTURE";
        const PROGRAM: &str = "ATERM_VERIFY_LAUNCHER_TARGO";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            let driver = CargoDriver {
                program: std::env::var_os(PROGRAM)
                    .expect("the parent's pinned targo")
                    .into(),
            };
            // The process inherited launcher metadata before the production constructor ran.
            let mut cmd = driver.command("build");
            cmd.args(["--offline", "--message-format=json"])
                .current_dir(&root);
            let output =
                aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
                    .expect("Cargo completed");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::fs::write(root.join("build.json"), output.stdout).unwrap();
            return;
        }
        let driver = cargo_driver().unwrap_or_else(|why| {
            aterm_spec::host::could_not_run(format_args!("pinned targo: {why}"))
        });
        let scratch = aterm_tempfile::tempdir().expect("isolated Cargo fixture");
        let root = scratch.path();
        write_launcher_fixture(root);
        let mut executable = None;
        for (launcher, want_fresh) in [("first", false), ("second", true)] {
            let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            cmd.args(["--exact", TEST, "--nocapture"])
                .current_dir(root)
                .env(ROOT, root)
                .env(PROGRAM, &driver.program)
                .env("CARGO_TARGET_DIR", root.join("target.noindex"))
                .env("CARGO_BUILD_BUILD_DIR", root.join("build.noindex"))
                .env("CARGO_MANIFEST_DIR", root.join(launcher))
                .env("OUT_DIR", root.join(launcher).join("out"));
            let output =
                aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
                    .expect("the nested Cargo test completed");
            assert!(
                output.status.success(),
                "{launcher}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let built = std::fs::read_to_string(root.join("build.json")).unwrap();
            let mut artifacts = 0;
            for line in built.lines() {
                let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
                    continue;
                };
                if row["reason"].as_str() == Some("compiler-artifact") {
                    assert_eq!(
                        row["fresh"].as_bool(),
                        Some(want_fresh),
                        "{launcher}: {row}"
                    );
                    artifacts += 1;
                    if row["target"]["name"].as_str() == Some("launcher_reuse_fixture")
                        && let Some(path) = row["executable"].as_str()
                    {
                        executable = Some(PathBuf::from(path));
                    }
                }
            }
            assert_eq!(
                artifacts, 3,
                "the fixture's build script, library and binary"
            );
        }
        let mut run = crate::exec::check_command(executable.expect("Cargo's built binary"));
        let runtime_out = root.join("runtime-output");
        run.env("CARGO_MANIFEST_DIR", root)
            .env("OUT_DIR", &runtime_out);
        super::super::bench_build::Targo::new(driver).environment(&mut run);
        let output = aterm_uds::deadline::output_within(&mut run, aterm_spec::wait::MUST_HAPPEN)
            .expect("the runtime fixture completed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("{}|{}", root.display(), runtime_out.display())
        );
    }

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
            lock: None,
            lock_refusal: None,
            lock_override: None,
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
        let dir = Some("build.build-dir='/w/target/build.noindex'");
        assert_eq!(
            d.display_led("build", dir, &["--release"]),
            "/s/targo --config build.build-dir='/w/target/build.noindex' --unverified build --release"
        );
        assert_eq!(d.display_led("metadata", dir, &[]), "/s/targo metadata");
    }
}
