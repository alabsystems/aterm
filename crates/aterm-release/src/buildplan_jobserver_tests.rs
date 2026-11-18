// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tiny Cargo consumers of the same validated pool attachment as the two Mac slices.
//! The stock consumer is the declared x86_64 compatibility cross-build boundary.

use super::*;
use std::time::Duration;

const GRANT: u32 = 3;
const CHILD: &str = "ATERM_RELEASE_JOBSERVER_PROBE";
const COMPLETE: &str = "jobserver-scalar-probe-complete";
const STOCK_REASON: &str =
    "PUBLIC BOUNDARY: x86_64-apple-darwin Cargo jobserver compatibility; Trust has no target std";

#[derive(Clone, Copy)]
enum Lane {
    Trust,
    Compat,
}

fn workspace() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../..")
}

fn package_prefix(tool_home: &Path) -> PathBuf {
    // The verifier declares installed tools before replacing HOME with private test state.
    // This is the same explicit, vetted store selection as ledger::selected_git.
    let configured = std::env::var_os("ATERM_TEST_PACKAGE_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            aterm_toolchain::atpkg_prefix(
                tool_home,
                std::env::var_os("XDG_CONFIG_HOME")
                    .as_deref()
                    .map(Path::new),
            )
        });
    atpkg::store::vet_prefix(Some(&configured), tool_home)
}

fn release_driver(
    lane: Lane,
    lock: &atpkg::trust_lock::TrustLock,
) -> (PathBuf, Option<atpkg::lease::Lease>) {
    match lane {
        Lane::Trust => {
            let tool_home = aterm_uds::host::tool_home().unwrap();
            let prefix = package_prefix(&tool_home);
            let bin = atpkg::trust_lock::build_dir(&prefix, lock.build)
                .join("bin")
                .canonicalize()
                .expect("the exact source's installed compiler slot");
            let lease = atpkg::lease::take_for_dir(&prefix, &bin, "Cargo jobserver fixture")
                .unwrap()
                .expect("the exact source's actual managed compiler lease");
            let compiler = crate::trust_lock::locked_prover(
                &prefix,
                lock,
                None,
                crate::trust_lock::Run::Cut,
                &mut |_, _| Err("the compiler fixture never installs another build".into()),
                &mut |selected| {
                    if selected != bin {
                        return Err("the compiler differs from the fixture's owned lease".into());
                    }
                    drop(
                        lease
                            .try_clone_descriptor()
                            .map_err(|error| error.to_string())?,
                    );
                    Ok(())
                },
            )
            .expect("the exact source's compiler is checked under its retained lease");
            assert_eq!(
                atpkg::trust_lock::compiler_commit(&compiler.bin)
                    .expect("the held compiler's unambiguous identity"),
                lock.compiler_commit
            );
            (compiler.bin.join("targo"), Some(lease))
        }
        Lane::Compat => {
            // A14's real compatibility path and its provider now select this source's role.
            if let Err(why) = require_rustup_target(&lock.stock_toolchain, X86_64) {
                aterm_spec::host::could_not_run(format!(
                    "the stock compat lane needs the lock's stock release {} with its {X86_64} std: {why} (fix: targo --unverified verify provision rustup)",
                    lock.stock_toolchain,
                ));
            }
            let version = stock_rustc_version(&lock.stock_toolchain)
                .expect("the actual stock compatibility identity");
            let pin = &lock.stock_toolchain;
            assert!(
                version
                    .lines()
                    .any(|line| line == format!("release: {pin}"))
            );
            (release_rustup_shim_dir().unwrap().join("cargo"), None)
        }
    }
}

fn run_probe(lane: Lane) {
    let git = crate::ledger::GitCli::new(workspace());
    let selected =
        crate::ledger::GitRunner::git_original(&git, &["rev-parse", "HEAD^{commit}"]).unwrap();
    assert!(selected.success());
    let source = selected.stdout_utf8().trim().to_owned();
    let lock = crate::release::CompilerPin::at(&git, &source)
        .unwrap()
        .lock();
    let lock_bytes = atpkg::trust_lock::render_body(&lock);
    // The real lease stays owned until both Cargo controls have positively completed.
    let (driver, _compiler_lease) = release_driver(lane, &lock);
    let sdk =
        atpkg::input_seal::macos_sdk().expect("the canonical Apple SDK for raw build drivers");
    for numeric_control in [false, true] {
        let dir = crate::test_scratch_dir("jobscalar");
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("trust.lock"), &lock_bytes).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='jobserver-scalar-probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = 'jobserver-scalar-probe'\nversion = '0.0.0'\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub const READY: bool = true;\n").unwrap();
        std::fs::write(
            root.join("build.rs"),
            r#"fn main() {
    let jobs = std::env::var("NUM_JOBS").unwrap();
    let configured = std::env::var("JOBSERVER_CONFIG_SENTINEL").unwrap();
    let sdk = std::path::PathBuf::from(std::env::var_os("SDKROOT").unwrap());
    assert!(sdk.is_absolute() && sdk.is_dir());
    std::fs::write(std::env::var_os("JOBSERVER_PROBE_OUTPUT").unwrap(),
        format!("{jobs}\n{configured}\n")).unwrap();
}
"#,
        )
        .unwrap();
        let config = root.join("sealed.toml");
        std::fs::write(
            &config,
            "[build]\njobs=99\n[env]\nJOBSERVER_CONFIG_SENTINEL='kept'\n",
        )
        .unwrap();
        let output_path = root.join("observed.txt");
        let mut command = command(&driver);
        command
            .env_clear()
            .env("HOME", std::env::var_os("HOME").unwrap())
            .env("PATH", release_tool_path(driver.parent().unwrap()).unwrap())
            .env("CARGO_HOME", root.join("cargo-home"))
            .env("CARGO_TARGET_DIR", root.join("target.noindex"))
            .env("TMPDIR", root)
            .env("SDKROOT", &sdk)
            .env("CARGO_BUILD_JOBS", "97")
            .env("JOBSERVER_PROBE_OUTPUT", &output_path)
            .current_dir(root);
        if matches!(lane, Lane::Trust) {
            command.arg("--unverified");
        }
        // Both CLI config files and inline config override environment defaults. The
        // sentinel proves inserting the grant did not discard the prefix config list.
        command.arg("--config").arg(&config).args([
            "--config",
            "build.jobs=98",
            "build",
            "--offline",
            "--locked",
            "--lib",
        ]);
        if matches!(lane, Lane::Compat) {
            command
                .env("ATERM_STOCK_REASON", STOCK_REASON)
                .env("RUSTUP_TOOLCHAIN", &lock.stock_toolchain)
                .env(
                    "RUSTUP_HOME",
                    release_rustup_home(&std::env::var_os("HOME").unwrap()).unwrap(),
                )
                .args(["--target", X86_64]);
        }
        let shares =
            aterm_capacity::BranchShares::new(&root.join("pool.jobs"), GRANT, 0, [true, true])
                .unwrap();
        let tree = shares.pool(true);
        let pool = aterm_capacity::Jobserver::adopt(&tree.handed_share(1).unwrap()).unwrap();
        assert_eq!((pool.slots(), pool.grant()), (1, GRANT));
        let implicit = pool.attach_cargo(&mut command, &|| false).unwrap();
        if numeric_control {
            command.args(["-j", &GRANT.to_string()]);
        }
        let output = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
            &mut command,
            Duration::from_secs(60),
            "the tiny Cargo jobserver scalar fixture",
        )
        .expect("the owned Cargo group completes");
        drop(implicit);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(std::fs::read_to_string(output_path).unwrap(), "3\nkept\n");
        let ignored_jobs = stderr
            .lines()
            .any(|line| line.contains("external jobserver") && line.contains("-j"));
        assert_eq!(ignored_jobs, numeric_control, "{stderr}");
        // The same live client can take the eventual full grant after build's tokens return.
        shares.finish(0);
        let returned: Vec<_> = std::iter::from_fn(|| pool.try_acquire().unwrap()).collect();
        assert_eq!(
            returned.len(),
            GRANT as usize,
            "no implicit or worker token was lost"
        );
    }
}

fn probe(lane: Lane, name: &str) {
    if let Some(selected) = std::env::var_os(CHILD) {
        assert_eq!(selected, name);
        assert_ne!(
            PathBuf::from(std::env::var_os("HOME").unwrap())
                .canonicalize()
                .unwrap(),
            aterm_uds::host::tool_home()
                .unwrap()
                .canonicalize()
                .unwrap(),
            "the real probe must exercise the verifier's disposable HOME"
        );
        run_probe(lane);
        println!("\n{COMPLETE}");
        return;
    }
    let tool_home = aterm_uds::host::tool_home().unwrap();
    let prefix = package_prefix(&tool_home);
    let rustup_home = release_rustup_home(&std::env::var_os("HOME").unwrap()).unwrap();
    let scratch = crate::test_scratch_dir("jobscalar-home");
    let home = scratch.path();
    // The parent retains ordinary admission through the positive wait of the whole probe.
    // A closed child HOME must neither manufacture release authority nor lose installed tools.
    let _claim = aterm_capacity::admit(
        aterm_capacity::Need::new("Cargo jobserver scalar contract")
            .cores(GRANT)
            .mem_gib(2),
        aterm_capacity::Admit::Wait,
    )
    .expect("normal admission for the tiny compiler fixture");
    let mut child = command(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, name)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("ATERM_TEST_TOOL_HOME", tool_home)
        .env("ATERM_TEST_PACKAGE_PREFIX", prefix)
        .env("RUSTUP_HOME", rustup_home);
    let output = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
        &mut child,
        Duration::from_secs(60),
        "the closed-home Cargo jobserver fixture",
    )
    .expect("the owned compiler fixture process completes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && let Some(why) = stdout
            .lines()
            .chain(stderr.lines())
            .find(|line| line.contains(aterm_spec::host::COULD_NOT_RUN))
    {
        aterm_spec::host::could_not_run(why);
    }
    assert!(output.status.success(), "{output:?}");
    assert!(
        stdout.lines().any(|line| line == COMPLETE),
        "the exact child must execute both real Cargo controls: {output:?}"
    );
}

#[test]
fn attached_trust_cargo_preserves_the_full_grant_without_a_numeric_jobs_warning() {
    probe(
        Lane::Trust,
        "buildplan::jobserver_tests::attached_trust_cargo_preserves_the_full_grant_without_a_numeric_jobs_warning",
    );
}

#[test]
fn attached_stock_compat_cargo_preserves_the_full_grant_without_a_numeric_jobs_warning() {
    probe(
        Lane::Compat,
        "buildplan::jobserver_tests::attached_stock_compat_cargo_preserves_the_full_grant_without_a_numeric_jobs_warning",
    );
}
