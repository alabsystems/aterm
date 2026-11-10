// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real frozen-tool fixture used by the dry-run controller's foreign-cutter test.

use super::*;

pub(crate) struct Fixture {
    pub root: aterm_tempfile::TempDir,
    pub repo: PathBuf,
    pub checkout: String,
    pub candidate: String,
}

impl Fixture {
    pub fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        std::fs::create_dir_all(repo.join("crates/aterm-release/src")).unwrap();
        std::fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=['crates/aterm-release']\nresolver='2'\n[workspace.package]\nversion='0.6.0'\n[profile.gate]\ninherits='dev'\n").unwrap();
        std::fs::write(
            repo.join("crates/aterm-release/Cargo.toml"),
            "[package]\nname='aterm-release'\nversion='0.0.0'\nedition='2021'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Cargo.lock"),
            "version=4\n[[package]]\nname='aterm-release'\nversion='0.0.0'\n",
        )
        .unwrap();
        let git = GitCli::new(&repo);
        let revision = |verdict, label| {
            std::fs::write(repo.join("crates/aterm-release/src/main.rs"), format!(
                "fn main() {{ if std::env::args().any(|a| a == \"--version\") {{ println!(\"aterm-release tool 1\"); }} else {{ println!(\"{label}\"); std::process::exit({verdict}); }} }}\n"
            )).unwrap();
            crate::ledger::git_ok(&git, &["add", "."]).unwrap();
            crate::ledger::git_ok(&git, &["commit", "-qm", label]).unwrap();
            crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout_utf8()
                .trim()
                .to_owned()
        };
        let checkout = revision(17, "checkout policy refuses");
        let candidate = revision(0, "R policy passes");
        crate::ledger::git_ok(&git, &["checkout", "-q", "--detach", &checkout]).unwrap();
        Self {
            root,
            repo,
            checkout,
            candidate,
        }
    }

    /// Production compiler checkout, frozen proof, and handover command. The controller
    /// supplies only the chosen revision; no release credentials or remote writes exist.
    pub fn verdict(&self, commit: &str) -> (i32, String) {
        let mut real = Real::new(&self.repo, Some(&self.repo), true, false).unwrap();
        real.release = self.root.path().join("release");
        real.state = self.root.path().join("state");
        real.build_inputs = || Ok("f".repeat(64));
        real.arm_inputs = |_, _| {};
        real.argv = vec!["release".into(), "--dry-run".into()];
        let prover = Prover {
            bin: crate::gates::trust_stage2_bin().unwrap(),
            commit: "0".repeat(40),
            source: "fixture Trust".into(),
        };
        let cutter = real.frozen_cutter(commit, &prover).unwrap();
        let mut command = real.cutter_command(&cutter.tool.path);
        let output =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
}

/// Each take is a fresh proved cutter process, so no process-local seal crosses modes.
#[test]
#[ignore = "explicit credential-free, source-only step 1 measurement"]
fn measure_same_main_step_one() {
    let repo = PathBuf::from(std::env::var_os("ATERM_DRY_TOOLS_REPO").expect("owned clean clone"));
    let engine =
        PathBuf::from(std::env::var_os("ATERM_DRY_TOOLS_ENGINE").expect("read-only engine source"));
    let base = std::env::var_os("ATERM_DRY_TOOLS_BASE").expect("input base");
    let mode = std::env::var("ATERM_DRY_TOOLS_MODE").expect("dry or real");
    assert!(matches!(mode.as_str(), "dry" | "real"));
    let mut real = Real::new(&repo, Some(&engine), true, false).unwrap();
    let commit = real.head();
    crate::release_tools::clean_at(&repo, &commit).unwrap();
    let prover = real.prover(None).unwrap();
    let started = std::time::Instant::now();
    let cutter = real.frozen_cutter(&commit, &prover).unwrap();
    let freeze = started.elapsed().as_secs_f64();
    real.argv = vec![
        "--step-one-source-measure".into(),
        mode.clone().into(),
        base,
        engine.into_os_string(),
    ];
    let mut command = real.cutter_command(&cutter.tool.path);
    for name in [
        "ATERM_DRY_TOOLS_REPO",
        "ATERM_DRY_TOOLS_ENGINE",
        "ATERM_DRY_TOOLS_BASE",
        "ATERM_DRY_TOOLS_MODE",
    ] {
        command.env_remove(name);
    }
    let output = real.output_forwarded(&mut command).unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success());
    println!(
        "step-one-launch {}",
        aterm_json::json!({
            "mode":mode, "commit":commit, "freeze_s":freeze,
            "wall_s":started.elapsed().as_secs_f64(),
        })
    );
}
