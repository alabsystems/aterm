// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual linked units, using the declared Trust driver without the SDK-exporting shim.

use super::*;

struct Observation {
    name: String,
    command: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    input_fingerprint: Option<Vec<u8>>,
}

struct FailedSdkFixture {
    directory: Option<aterm_tempfile::TempDir>,
    observations: std::cell::RefCell<Vec<Observation>>,
}

impl Drop for FailedSdkFixture {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(directory) = self.directory.take()
        {
            let root = directory.keep();
            // Publish diagnostics only after failure, never between compiler freshness takes.
            let evidence = root.join("evidence");
            let _ = std::fs::create_dir(&evidence);
            for observed in self.observations.borrow().iter() {
                for (extension, bytes) in [
                    ("context", observed.command.as_bytes()),
                    ("json", &observed.stdout),
                    ("stderr", &observed.stderr),
                ] {
                    let _ = std::fs::write(
                        evidence.join(format!("{}.{}", observed.name, extension)),
                        bytes,
                    );
                }
                if let Some(bytes) = &observed.input_fingerprint {
                    let _ = std::fs::write(
                        evidence.join(format!("{}.input-fingerprint.json", observed.name)),
                        bytes,
                    );
                }
            }
            eprintln!("SDK linked fixture retained at {}", root.display());
        }
    }
}

fn complete(command: &mut Command) -> std::process::Output {
    let output = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
        command,
        std::time::Duration::from_secs(60),
        "the tiny linked SDK reuse fixture",
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

// Pinned Cargo's Message::Run prints this line at verbose level. Wrappers themselves
// make native tool-input facts unavailable, so observe the owning dispatch without one.
fn dispatched(stderr: &[u8]) -> Vec<&'static str> {
    let text = std::str::from_utf8(stderr).unwrap();
    let mut units = Vec::new();
    for line in text
        .lines()
        .filter(|line| line.trim_start().starts_with("Running `"))
    {
        for name in ["build_script_build", "sdk_fresh_subject"] {
            if line.contains(&format!("--crate-name {name} ")) {
                units.push(name);
            }
        }
    }
    units.sort_unstable();
    units
}

#[test]
fn captured_sdk_reuses_linked_binary_and_build_script_without_compiler_dispatch() {
    let workspace = aterm_tempfile::manifest_dir().join("../..");
    let git = crate::ledger::GitCli::new(&workspace);
    let selected =
        crate::ledger::GitRunner::git_original(&git, &["rev-parse", "--verify", "HEAD^{commit}"])
            .unwrap();
    assert!(selected.success());
    let selected = selected.stdout_utf8().trim().to_owned();
    let lock = crate::release::CompilerPin::at(&git, &selected)
        .unwrap()
        .lock();
    let home = std::env::var_os("HOME").unwrap();
    let tool_home = aterm_uds::host::tool_home().unwrap();
    // The verifier declares the installed tools before replacing HOME with private state.
    let configured = std::env::var_os("ATERM_TEST_PACKAGE_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            aterm_toolchain::atpkg_prefix(
                &tool_home,
                std::env::var_os("XDG_CONFIG_HOME")
                    .as_deref()
                    .map(Path::new),
            )
        });
    let prefix = atpkg::store::vet_prefix(Some(&configured), &tool_home);
    let bin = match atpkg::trust_lock::standing(&prefix, &lock) {
        atpkg::trust_lock::LockedBuild::Present { bin } => bin,
        other => panic!("the declared Trust build must be installed: {other:?}"),
    };
    let bin = bin.canonicalize().unwrap();
    let _lease = atpkg::lease::take_for_dir(&prefix, &bin, "linked SDK reuse fixture")
        .unwrap()
        .expect("the declared compiler has a real managed lease");
    let prover = crate::trust_lock::locked_prover(
        &prefix,
        &lock,
        None,
        crate::trust_lock::Run::Cut,
        &mut |_, _| Err("the compiler fixture never installs another build".into()),
        &mut |selected| {
            if selected == bin.as_path() {
                Ok(())
            } else {
                Err("the compiler slot differs from the fixture's owned lease".into())
            }
        },
    )
    .unwrap();
    let sdk = crate::build_inputs::AppleSdk::capture().unwrap();
    // A few standalone compiler units, through the same normal ledger as other tests.
    let _claim = aterm_capacity::admit(
        aterm_capacity::Need::new("linked SDK reuse fixture")
            .cores(2)
            .mem_gib(2),
        aterm_capacity::Admit::Wait,
    )
    .unwrap();
    // Declared-input windows retain ancestor stamps, including the shared temporary
    // directory. Use the binary-adjacent namespace that the launcher reuse fixture uses.
    let binary = std::env::current_exe().unwrap();
    let fixtures = binary.parent().unwrap().join("sdk-reuse-fixtures.noindex");
    std::fs::create_dir_all(&fixtures).unwrap();
    let scratch = FailedSdkFixture {
        directory: Some(
            aterm_tempfile::Builder::new()
                .prefix("sdk-")
                .tempdir_in(fixtures)
                .unwrap(),
        ),
        observations: Default::default(),
    };
    // Cargo records its lexical package root, while the compiler observes the kernel cwd.
    // Use one spelling before either builds: macOS's /var alias resolves under /private/var.
    let root = scratch
        .directory
        .as_ref()
        .unwrap()
        .path()
        .canonicalize()
        .unwrap();
    let nonce = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
        root.as_os_str().as_encoded_bytes(),
    ));
    for explicit in [false, true] {
        let tree = root.join(if explicit { "captured" } else { "omitted" });
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(
            tree.join("Cargo.toml"),
            "[package]\nname='sdk-fresh-subject'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            tree.join("src/main.rs"),
            format!("fn main() {{ println!(\"sdk fixture {nonce}\"); }}\n"),
        )
        .unwrap();
        std::fs::write(
            tree.join("build.rs"),
            format!("fn main() {{ println!(\"cargo:rerun-if-changed=build.rs\"); println!(\"cargo:rustc-env=SDK_FIXTURE={nonce}\"); }}\n"),
        )
        .unwrap();
        let env = ReleaseEnv {
            sdk: &sdk,
            path: release_tool_path(&prover.bin).unwrap(),
            home: &home,
            tmp: &tree,
            cargo_home: tree.join("cargo-home"),
            build_number: 0,
            short_commit: "",
            commit: "",
            target_dir: tree.join("target/.ubd/linked.noindex"),
            stock: None,
            index_hint: None,
            units: None,
        };
        let calls = std::cell::Cell::new(0);
        let build = || {
            let mut command = release_build_command(
                &prover.bin.join("targo"),
                [
                    "--unverified",
                    "build",
                    "--offline",
                    "--message-format=json",
                    "--verbose",
                    "--verbose",
                    "--color=never",
                    "--config",
                    "profile.dev.incremental=false",
                    "--manifest-path",
                ]
                .into_iter()
                .map(OsString::from)
                .chain([tree.join("Cargo.toml").into_os_string()])
                .collect(),
                &env,
            );
            command.env("CARGO_BUILD_JOBS", "2");
            command.env(
                "CARGO_LOG",
                "targo::compiler::fingerprint=info,targo::compiler::fingerprint::build_script_inputs=debug",
            );
            if !explicit {
                // The old producer: same declared compiler, source and layout, no SDK input.
                command.env_remove("SDKROOT");
            }
            let call = calls.get();
            calls.set(call + 1);
            let context = format!("{command:?}\n");
            sdk.verify().unwrap();
            let output = complete(&mut command);
            // Read into memory only: publishing diagnostics between takes could itself
            // invalidate a declared-input window. The second build overwrites this record.
            let input_fingerprint = output
                .stdout
                .split(|b| *b == b'\n')
                .filter_map(|line| aterm_json::from_slice::<aterm_json::Value>(line).ok())
                .find(|row| row["reason"].as_str() == Some("build-script-executed"))
                .and_then(|row| {
                    let out = Path::new(row["out_dir"].as_str()?);
                    crate::durable::read_regular_bytes(
                        &out.parent()?
                            .join("fingerprint/run-build-script-build-script-build.json"),
                        64 * 1024,
                    )
                    .ok()
                });
            scratch.observations.borrow_mut().push(Observation {
                name: format!("{}-{call}", if explicit { "captured" } else { "omitted" }),
                command: context,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                input_fingerprint,
            });
            sdk.verify().unwrap();
            output
        };
        let first = build();
        assert_eq!(
            dispatched(&first.stderr),
            ["build_script_build", "sdk_fresh_subject"],
            "each first build must actually dispatch both linked units"
        );
        let second = build();
        let artifacts: Vec<aterm_json::Value> = second
            .stdout
            .split(|b| *b == b'\n')
            .filter_map(|line| aterm_json::from_slice(line).ok())
            .filter(|row: &aterm_json::Value| row["reason"].as_str() == Some("compiler-artifact"))
            .collect();
        let mut roles = std::collections::BTreeMap::new();
        for artifact in artifacts {
            let kinds = artifact["target"]["kind"].as_array().unwrap();
            assert_eq!(kinds.len(), 1);
            let role = kinds[0].as_str().unwrap();
            assert!(matches!(role, "bin" | "custom-build"));
            assert!(
                roles
                    .insert(role.to_owned(), artifact["fresh"].as_bool().unwrap())
                    .is_none()
            );
        }
        assert_eq!(
            roles,
            std::collections::BTreeMap::from([
                ("bin".into(), explicit),
                ("custom-build".into(), explicit),
            ]),
            "fixture={}\n{}",
            tree.display(),
            String::from_utf8_lossy(&second.stderr),
        );
        assert_eq!(
            dispatched(&second.stderr),
            if explicit {
                Vec::new()
            } else {
                vec!["build_script_build", "sdk_fresh_subject"]
            },
        );
        assert_eq!(
            complete(&mut command(
                tree.join("target/.ubd/linked.noindex/debug/sdk-fresh-subject")
            ))
            .stdout,
            format!("sdk fixture {nonce}\n").as_bytes()
        );
    }
}
