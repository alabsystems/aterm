// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local harness artifacts with private Git histories and mocked compiler/executable probes.

use super::*;
use atpkg_publish::artifact::pack::HARNESS_VERSION_ENV;
use atpkg_publish::host::Slug;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;

#[derive(Clone, Copy)]
enum Damage {
    None,
    Version,
    ToolName,
    WritableCompilerOutput,
    Copy,
    CompilerOutput,
    SourceBeforeCopy,
    SourceAfterProbe,
}

fn harness_world(label: &str) -> World {
    let mut w = world(label);
    w.checkout = w.root.join("claude-code-alab");
    fixture_checkout(
        &w.checkout,
        &[
            (
                "Cargo.toml",
                b"[package]\nname = \"claude-code-alab\"\nversion = \"0.1.0\"\n",
            ),
            (".gitignore", b"/target/\n/hidden\n"),
        ],
    );
    let bare = w
        .root
        .join("remotes")
        .join(&w.ctx.accounts.staging)
        .join("claude-code-alab.git");
    super::super::fixture::bare_origin(&bare);
    git(
        &w.checkout,
        &["remote", "add", "origin", &bare.to_string_lossy()],
    );
    git(
        &w.checkout,
        &["push", "-q", "origin", "HEAD:refs/heads/main"],
    );
    w.host.add_clone_source(
        &Slug::new(&w.ctx.accounts.staging, "claude-code-alab"),
        &bare,
    );
    w
}

fn request(w: &World) -> Request {
    Request {
        program: "claude-code-alab".into(),
        policy: Policy::Unverified,
        out: Some(w.ctx.home.join(".ubd/local-harness.noindex")),
        ..w.request()
    }
}

fn harness_tools(w: &World, damage: Damage) -> FakeTools {
    let source = git(&w.checkout, &["rev-parse", "HEAD"]);
    let version = format!(
        "0.1.0+g{}",
        git(&w.checkout, &["rev-parse", "--short=9", "HEAD"])
    );
    let checkout = w.checkout.clone();
    let original = Mutex::new(None::<PathBuf>);
    let mut tools = FakeTools::new(HOST);
    tools.answer = Box::new(move |run| {
        let args = run.argv();
        if args.first().map(String::as_str) == Some("metadata") {
            return Some(ran(
                0,
                r#"{"packages":[{"name":"claude-code-alab","version":"0.1.0","manifest_path":"/x/Cargo.toml"}],"metadata":{"atpkg":{"expose":["claude-alab"]}}}"#,
                "",
            ));
        }
        if args.iter().any(|a| a == "build") {
            assert_eq!(
                run.env_of(HARNESS_VERSION_ENV).and_then(|s| s.to_str()),
                Some(version.as_str())
            );
            assert!(
                run.env_of("CARGO_TARGET_DIR")
                    .unwrap()
                    .to_string_lossy()
                    .contains(&source)
            );
            let exe = put_exe(
                &PathBuf::from(run.env_of("CARGO_TARGET_DIR").unwrap()).join("release/claude-alab"),
                &x86("stamped fixture"),
            );
            *original.lock().unwrap() = Some(exe.clone());
            if matches!(damage, Damage::WritableCompilerOutput) {
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
            }
            if matches!(damage, Damage::SourceBeforeCopy) {
                put(&checkout.join("new-input"), b"uncommitted build input");
            }
            return Some(ran(
                0,
                &format!(
                    "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"claude-alab\",\"kind\":[\"bin\"]}},\"executable\":\"{}\"}}\n",
                    exe.display()
                ),
                "",
            ));
        }
        if args == ["--version"]
            && run
                .program
                .file_name()
                .is_some_and(|name| name == "claude-alab")
        {
            assert!(
                run.clear_env,
                "version proof inherits no transfer or launch markers"
            );
            assert_eq!(
                std::fs::metadata(&run.program)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            match damage {
                Damage::Copy => {
                    put_exe(&run.program, &x86("changed staged bytes"));
                }
                Damage::CompilerOutput => {
                    put_exe(
                        original.lock().unwrap().as_ref().unwrap(),
                        &x86("changed compiler output"),
                    );
                }
                Damage::SourceAfterProbe => {
                    put(
                        &checkout.join("new-input"),
                        b"changed at the proof boundary",
                    );
                }
                _ => {}
            }
            let reported = if matches!(damage, Damage::Version) {
                "0.1.0"
            } else {
                &version
            };
            let printed = if matches!(damage, Damage::ToolName) {
                "claude-alab"
            } else {
                "claude-code-alab"
            };
            return Some(ran(
                0,
                // Claude a1fe cli::version_line prints REPO, not its TOOL name.
                &format!("{printed} {reported} (claude not installed)\n"),
                "",
            ));
        }
        None
    });
    tools
}

#[test]
fn a_local_harness_has_one_main_identity_stamp_and_payload_receipt() {
    let w = harness_world("local-harness-identity");
    let req = request(&w);
    let tools = harness_tools(&w, Damage::None);
    // Trust 15698's recorded compiler commit, supplied through the mock -vV response.
    let compiler = "20565c3255adf151f9eb920e89cdf83c38f84abe";
    tools.version(
        &w.bin.join("trustc"),
        &super::super::fixture::version_text(compiler, HOST),
    );
    let plan = build_local::plan(&w.ctx, &req, &tools).unwrap();
    assert_eq!(plan.compiler, compiler);
    let identity = plan.harness.as_ref().unwrap();
    assert_eq!(identity.source, git(&w.checkout, &["rev-parse", "HEAD"]));
    assert_eq!(identity.build_number, 1);
    assert_eq!(
        plan.command
            .argv()
            .iter()
            .filter(|a| *a == "--locked")
            .count(),
        1
    );
    assert!(!plan.command.argv().iter().any(|a| a == "--target"));
    assert!(plan.command.env_of("RUSTFLAGS").is_none());
    assert!(
        plan.command
            .env_remove
            .iter()
            .any(|key| key == HARNESS_VERSION_ENV)
    );
    let built = build_local::build(&w.ctx, &req, &plan, &tools).unwrap();
    assert!(
        built
            .dest
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&identity.source)
    );
    let receipt: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(built.dest.join("local-build.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["schema"].as_u64(), Some(2));
    assert_eq!(receipt["origin"].as_str(), Some("local-build"));
    assert_eq!(receipt["source"].as_str(), Some(identity.source.as_str()));
    assert_eq!(receipt["build_number"].as_u64(), Some(1));
    assert_eq!(receipt["version"].as_str(), Some(identity.version.as_str()));
    assert_eq!(receipt["source_policy"].as_str(), Some("clean-main"));
    assert_eq!(receipt["command"].as_str(), Some("claude-alab"));
    assert_eq!(receipt["compiler"].as_str(), Some(compiler));
    let exe = built.dest.join("bin/claude-alab");
    let bin_dir = built.dest.join("bin");
    for private in [&plan.out, &built.dest, &bin_dir, &built.logs] {
        assert_eq!(
            std::fs::metadata(private).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(built.dest.join("local-build.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        receipt["executables"][0]["path"].as_str(),
        Some("bin/claude-alab")
    );
    let expected_sha = atpkg::sha256_file(&exe).unwrap();
    assert_eq!(
        receipt["executables"][0]["sha256"].as_str(),
        Some(expected_sha.as_str())
    );
    assert_eq!(
        receipt["executables"][0]["size"].as_u64(),
        Some(std::fs::metadata(&exe).unwrap().len())
    );
    let before = std::fs::read(built.dest.join("local-build.json")).unwrap();
    let builds = tools.runs_of("build").len();
    assert!(build_local::build(&w.ctx, &req, &plan, &tools).is_err());
    assert_eq!(
        tools.runs_of("build").len(),
        builds,
        "existing artifact refuses before rebuilding"
    );
    assert_eq!(
        std::fs::read(built.dest.join("local-build.json")).unwrap(),
        before
    );
}

#[test]
fn malformed_harness_compilers_refuse_before_output_build_or_artifact_probe() {
    let w = harness_world("local-harness-compiler-refusal");
    let req = request(&w);
    let good = "20565c3255adf151f9eb920e89cdf83c38f84abe";
    let fields = [
        "".to_string(),
        "commit-hash: unknown\n".to_string(),
        "commit-hash: 20565c325\n".to_string(),
        format!("commit-hash: {good}0\n"),
        format!("commit-hash: {}\n", good.to_ascii_uppercase()),
        format!("commit-hash: {}\n", "g".repeat(40)),
        "commit-hash: ../../foreign\n".to_string(),
        "commit-hash: /foreign/compiler\n".to_string(),
        format!("commit-hash: {good}\t\n"),
        format!("commit-hash: {good}\0\n"),
        format!("commit-hash:  {good}\n"),
        format!("commit-hash:{good}\n"),
        format!("commit-hash: {good}\ncommit-hash: {good}\n"),
    ];
    for field in fields {
        let tools = harness_tools(&w, Damage::None);
        tools.version(
            &w.bin.join("trustc"),
            &format!("binary: trustc\n{field}host: {HOST}\n"),
        );
        let error = build_local::plan(&w.ctx, &req, &tools).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("full lowercase 40-hex commit-hash")
        );
        assert!(tools.runs_of("metadata").is_empty());
        assert!(tools.runs_of("build").is_empty());
        assert!(tools.runs_of("--version").is_empty());
        assert!(!req.out.as_ref().unwrap().exists());
    }

    let tools = harness_tools(&w, Damage::None);
    let plan = build_local::plan(&w.ctx, &req, &tools).unwrap();
    for compiler in ["unknown", "../../foreign", good] {
        let mut altered = plan.clone();
        altered.compiler = compiler.into();
        let error = build_local::build(&w.ctx, &req, &altered, &tools).unwrap_err();
        assert!(error.to_string().contains("plan's compiler differs"));
        assert!(tools.runs_of("build").is_empty());
        assert!(tools.runs_of("--version").is_empty());
        assert!(!req.out.as_ref().unwrap().exists());
    }
}

#[test]
fn local_harness_source_proof_rejects_hidden_inputs_and_unpublished_commits() {
    for case in ["tracked", "untracked", "ignored", "hint", "unpublished"] {
        let w = harness_world(&format!("local-harness-source-{case}"));
        match case {
            "tracked" => put(&w.checkout.join("Cargo.toml"), b"changed input"),
            "untracked" => put(&w.checkout.join("new-input"), b"new input"),
            "ignored" => put(&w.checkout.join("hidden"), b"ignored input"),
            "hint" => {
                git(
                    &w.checkout,
                    &["update-index", "--assume-unchanged", "Cargo.toml"],
                );
            }
            "unpublished" => {
                put(&w.checkout.join("new-input"), b"unpushed source");
                git(&w.checkout, &["add", "new-input"]);
                git(&w.checkout, &["commit", "-q", "-m", "unpushed"]);
            }
            _ => unreachable!(),
        }
        let tools = harness_tools(&w, Damage::None);
        assert!(
            build_local::plan(&w.ctx, &request(&w), &tools).is_err(),
            "{case}"
        );
        assert!(tools.runs_of("build").is_empty(), "{case}");
    }
}

#[test]
fn failed_harness_identity_or_payload_proof_never_commits_an_artifact_receipt() {
    for (case, damage) in [
        ("version", Damage::Version),
        ("tool-name", Damage::ToolName),
        ("writable-compiler-output", Damage::WritableCompilerOutput),
        ("copy", Damage::Copy),
        ("compiler-output", Damage::CompilerOutput),
        ("source-before", Damage::SourceBeforeCopy),
        ("source-after", Damage::SourceAfterProbe),
    ] {
        let w = harness_world(&format!("local-harness-proof-{case}"));
        let req = request(&w);
        let tools = harness_tools(&w, damage);
        let plan = build_local::plan(&w.ctx, &req, &tools).unwrap();
        assert!(
            build_local::build(&w.ctx, &req, &plan, &tools).is_err(),
            "{case}"
        );
        let root = plan.out.join("artifacts/claude-code-alab");
        if root.exists() {
            for entry in std::fs::read_dir(root).unwrap() {
                let candidate = entry.unwrap().path();
                assert!(!candidate.join("local-build.json").exists(), "{case}");
                assert!(
                    candidate.join("bin/claude-alab").exists(),
                    "failed image remains in place: {case}"
                );
            }
        }
    }
}

#[test]
fn unsafe_existing_output_ancestors_are_refused_without_repair_or_probe() {
    for case in ["group-write", "other-write", "symlink"] {
        let w = harness_world(&format!("local-harness-parent-{case}"));
        let mut req = request(&w);
        let actual = w.ctx.home.join("foreign-output-parent");
        aterm_uds::private_file::create_private_dir(&actual).unwrap();
        put(&actual.join("owner-note"), b"preserve this folder");
        let parent = if case == "symlink" {
            let link = w.ctx.home.join("linked-output-parent");
            std::os::unix::fs::symlink(&actual, &link).unwrap();
            link
        } else {
            let mode = if case == "group-write" { 0o770 } else { 0o707 };
            std::fs::set_permissions(&actual, std::fs::Permissions::from_mode(mode)).unwrap();
            actual.clone()
        };
        req.out = Some(parent.join("artifacts.noindex"));
        let before = std::fs::metadata(&actual).unwrap().permissions().mode();
        let tools = harness_tools(&w, Damage::None);
        let plan = build_local::plan(&w.ctx, &req, &tools).unwrap();
        assert!(
            build_local::build(&w.ctx, &req, &plan, &tools).is_err(),
            "{case}"
        );
        assert!(tools.runs_of("build").is_empty(), "{case}");
        assert!(tools.runs_of("--version").is_empty(), "{case}");
        assert!(!actual.join("artifacts.noindex").exists(), "{case}");
        assert_eq!(
            std::fs::metadata(&actual).unwrap().permissions().mode(),
            before,
            "{case}"
        );
        assert_eq!(
            std::fs::read(actual.join("owner-note")).unwrap(),
            b"preserve this folder"
        );
    }
}

#[test]
fn a_harness_artifact_reserved_during_the_build_is_never_replaced() {
    let w = harness_world("local-harness-reservation");
    let req = request(&w);
    let mut tools = harness_tools(&w, Damage::None);
    let plan = build_local::plan(&w.ctx, &req, &tools).unwrap();
    let dest = plan.out.join("artifacts/claude-code-alab").join(format!(
        "{}-{}-{HOST}-release-unverified",
        plan.source, plan.compiler
    ));
    let incoming = dest.clone();
    let answer = std::mem::replace(&mut tools.answer, Box::new(|_| None));
    tools.answer = Box::new(move |run| {
        if run.argv().iter().any(|word| word == "build") {
            std::fs::create_dir_all(incoming.parent().unwrap()).unwrap();
            std::fs::create_dir(&incoming).unwrap();
            put(
                &incoming.join("owner-note"),
                b"another writer owns this name",
            );
        }
        answer(run)
    });
    assert!(build_local::build(&w.ctx, &req, &plan, &tools).is_err());
    assert_eq!(
        std::fs::read(dest.join("owner-note")).unwrap(),
        b"another writer owns this name"
    );
    assert!(!dest.join("local-build.json").exists());
    assert!(tools.runs_of("--version").is_empty());
}
