// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn candidate_listing_uses_the_selected_git_provider() {
    let env = EnvSnapshot::capture();
    let dir = aterm_tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut cmd = crate::git_provider::command();
        cmd.current_dir(dir.path()).args(args);
        for name in crate::changed::GIT_REDIRECTS {
            cmd.env_remove(name);
        }
        let out =
            aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(dir.path().join("untracked"), "not a source input\n").unwrap();
    run(&["add", "--", "Cargo.toml"]);
    assert_eq!(
        git_files(dir.path(), &env).unwrap(),
        [PathBuf::from("Cargo.toml")]
    );
}

#[test]
fn installed_release_rejects_config_target_and_accepts_jobs() {
    let env = EnvSnapshot::capture();
    let rig = Rig::new(&env).expect("installed released shim and helper");
    io(fs::create_dir_all(rig.tree.join("src"))).unwrap();
    io(fs::create_dir_all(rig.tree.join(".cargo"))).unwrap();
    io(fs::write(
        rig.tree.join("Cargo.toml"),
        format!("[package]\nname='aterm-time'\nversion='0.0.0'\nedition='2024'\n[workspace]\n[workspace.package]\nversion='{}'\n", rig.version.trim_start_matches("aterm ")),
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"aterm-time\"\nversion = \"0.0.0\"\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("src/lib.rs"),
        "pub fn bootstrap_probe() {}\n",
    ))
    .unwrap();
    // Exact regression introduced by ff1626d7ab8, exercised by the release pair.
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"target/.ubd/main.noindex\"\n",
    ))
    .unwrap();
    let bad = rig.build().unwrap();
    let said = String::from_utf8_lossy(&bad.stderr);
    if rig.version == "aterm 0.197.0" {
        assert!(!bad.status.success(), "0.197.0 must reject ff1626d7ab8");
    }
    // Later public releases may support the layout; the 0.197.0 pair must refuse it.
    if !bad.status.success() {
        assert!(
            said.contains("explicit build target is unmanaged or unsupported"),
            "{said}"
        );
    }
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 1\n",
    ))
    .unwrap();
    let good = rig.build().unwrap();
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert!(rig.tree.join("target/debug/libaterm_time.rlib").is_file());
    assert!(built_artifact(&good.stdout, &rig.home));

    // The same production stage must name the changed input, rather than merely fail
    // an unrelated source build. Commit the accepted config, then reproduce ff1626d7ab8.
    for args in [
        vec!["init", "-q"],
        vec![
            "add",
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "src/lib.rs",
        ],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "accepted bootstrap",
        ],
    ] {
        let mut command = crate::git_provider::command();
        command.current_dir(&rig.tree).args(args);
        let out = io(aterm_uds::deadline::output_within(
            &mut command,
            Duration::from_secs(60),
        ))
        .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut command = crate::git_provider::command();
    command.current_dir(&rig.tree).args([
        "tag",
        &format!("v{}", rig.version.trim_start_matches("aterm ")),
    ]);
    let out = io(aterm_uds::deadline::output_within(
        &mut command,
        Duration::from_secs(60),
    ))
    .unwrap();
    assert!(out.status.success());
    let context = crate::Ctx::new(
        rig.tree.clone(),
        crate::Scope::changed("HEAD", Vec::new(), false),
        env,
        rig.home.join("run"),
    );
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"target/.ubd/main.noindex\"\n",
    ))
    .unwrap();
    let mut red = crate::ladder::Report::new("released-shim regression");
    super::super::run(&context, &mut red);
    if rig.version == "aterm 0.197.0" {
        assert!(
            red.outcomes().any(|(o, label)| o
                == crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed)
                && label.contains(".cargo/config.toml")
                && label.contains("explicit build target is unmanaged or unsupported")),
            "{}",
            red.render()
        );
    }
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 2\n",
    ))
    .unwrap();
    let mut green = crate::ladder::Report::new("released-shim negative control");
    super::super::run(&context, &mut green);
    assert!(
        green
            .outcomes()
            .any(|(o, label)| o == crate::ladder::Outcome::Ok
                && label.contains("built aterm-time")),
        "{}",
        green.render()
    );

    // A source-only edit must cost no snapshot, no release lookup and no build.
    io(fs::write(
        rig.tree.join(".cargo/config.toml"),
        "[build]\njobs = 1\n",
    ))
    .unwrap();
    io(fs::write(
        rig.tree.join("src/lib.rs"),
        "pub fn changed() {}\n",
    ))
    .unwrap();
    let mut untouched = crate::ladder::Report::new("no bootstrap change");
    super::super::run(&context, &mut untouched);
    assert!(
        untouched
            .outcomes()
            .any(|(o, label)| o == crate::ladder::Outcome::Ok && label.contains("no child build")),
        "{}",
        untouched.render()
    );
}

#[test]
fn sandbox_only_allows_scratch_and_resource_bookkeeping_writes() {
    let env = EnvSnapshot::capture();
    let rig = Rig::new(&env).unwrap();
    let outside = aterm_tempfile::tempdir().unwrap();
    for (path, accepted) in [
        (outside.path().join("protected"), false),
        (rig.home.join("allowed"), true),
    ] {
        let mut cmd = rig.sandbox().unwrap();
        cmd.arg("/usr/bin/touch").arg(&path);
        let out = io(aterm_uds::deadline::output_within(
            &mut cmd,
            Duration::from_secs(60),
        ))
        .unwrap();
        assert_eq!(
            out.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(path.exists(), accepted);
    }
}
