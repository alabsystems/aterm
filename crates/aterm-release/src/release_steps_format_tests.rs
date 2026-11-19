// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn formatting_reads_selected_r_instead_of_the_clean_controller_and_records_no_pass() {
    use crate::ledger::GitRunner as _;

    let workspace = aterm_tempfile::manifest_dir().join("../..");
    let workspace_git = crate::ledger::GitCli::new(&workspace);
    let selected = workspace_git
        .git_original(&["rev-parse", "HEAD^{commit}"])
        .unwrap();
    assert!(selected.success());
    let source = selected.stdout_utf8().trim().to_owned();
    let pin = crate::trust_lock::CompilerPin::at(&workspace_git, &source).unwrap();
    let lock = workspace_git
        .git_original(&["show", &format!("{source}:trust.lock")])
        .unwrap();
    assert!(lock.success());

    let tool_home = aterm_uds::host::tool_home().unwrap();
    let prefix = std::env::var_os("ATERM_TEST_PACKAGE_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            aterm_toolchain::atpkg_prefix(
                &tool_home,
                std::env::var_os("XDG_CONFIG_HOME")
                    .as_deref()
                    .map(Path::new),
            )
        });
    let store = crate::trust_lock::ManagedStoreContext::declared(&tool_home, &prefix).unwrap();
    assert!(
        matches!(
            atpkg::trust_lock::standing(store.prefix(), &pin.lock()),
            atpkg::trust_lock::LockedBuild::Present { .. }
        ),
        "the declared compiler must already be provisioned"
    );

    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname='snapshot-format-subject'\nversion='0.207.0'\nedition='2024'\n[workspace]\n").unwrap();
    std::fs::write(
        repo.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"snapshot-format-subject\"\nversion = \"0.207.0\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("trust.lock"), &lock.stdout).unwrap();
    std::fs::write(repo.join("rustfmt.toml"), "edition = \"2024\"\n").unwrap();
    std::fs::write(
        repo.join("rust-toolchain.toml"),
        "[toolchain]\nchannel='trust'\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn value()->u8{7}\n").unwrap();
    let git = crate::ledger::GitCli::new(&repo);
    let commit = |message: &str| {
        crate::ledger::git_ok(&git, &["add", "."]).unwrap();
        crate::ledger::git_ok(
            &git,
            &["-c", "commit.gpgsign=false", "commit", "-qm", message],
        )
        .unwrap();
        crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_owned()
    };
    let bad = commit("selected unformatted R");
    let good_text = "pub fn value() -> u8 {\n    7\n}\n";
    std::fs::write(repo.join("src/lib.rs"), good_text).unwrap();
    let good = commit("clean formatted controller A");

    let mut real = Real::new(&repo, None, true, false).unwrap();
    real.compiler_store = store;
    real.release = temp.path().join("release");
    real.state = temp.path().join("state");
    real.build_inputs = |_, _| Ok("a".repeat(64));
    real.checkout_build = |_, _, _, _| panic!("formatting must start no build");
    real.engine_origin = |_, _| panic!("formatting must read no engine");
    let prover = real.candidate_prover(&bad, None).unwrap();
    let finding = check(&real, &bad, &prover).unwrap_err();
    assert!(matches!(finding, InputFinding::Failed(ref detail) if detail.contains("src/lib.rs")));
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        good_text
    );
    assert_eq!(
        crate::ledger::git_ok(&git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim(),
        good
    );
    let input_git = crate::ledger::GitCli::new(crate::input_record::directory(&real.release));
    assert_eq!(
        crate::ledger::git_ok(&input_git, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout_utf8()
            .trim(),
        bad
    );

    assert!(
        matches!(
            check(&real, &good, &prover),
            Err(InputFinding::Unanswered(_))
        ),
        "a previous candidate's held context cannot judge another source"
    );
    let prover = real.candidate_prover(&good, None).unwrap();
    check(&real, &good, &prover).unwrap();
    assert!(
        !real.release.join(crate::check_record::DIR).exists(),
        "the prefix must issue no qualification or release-input receipt"
    );
}
