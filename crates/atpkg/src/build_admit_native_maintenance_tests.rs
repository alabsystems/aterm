// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::process::ExitStatusExt as _;

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}

#[test]
fn only_original_native_prune_commands_qualify_for_a_capability_probe() {
    for args in [
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--",
            "repo-prune",
            "--tool-target-trees",
            "--rustc-caches",
            "--dry-run",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--cpus",
            "2",
            "--slots=1",
            "--memory-mib",
            "8192",
            "--exclusive",
            "--wait-seconds=30",
            "--",
            "repo-prune",
            "--host",
            "native",
        ],
    ] {
        assert!(command(&words(&args), Tool::Targo, "-"), "{args:?}");
        for tool in [Tool::Cargo, Tool::CargoCompat, Tool::Tippy] {
            assert!(!command(&words(&args), tool, "-"));
        }
        for placement in ["config", "env", "unknown", ""] {
            assert!(!command(&words(&args), Tool::Targo, placement));
        }
    }
}

#[test]
fn aliases_other_jobs_and_malformed_fixed_arguments_keep_cache_observation() {
    for args in [
        vec!["resource", "fixed", "--", "repo-prune"],
        vec!["trust", "resource", "fixed", "--", "repo-prune"],
        vec!["--unverified", "resource", "exec", "--", "repo-prune"],
        vec!["--unverified", "resource", "fixed", "--", "build"],
        vec!["--unverified", "resource", "fixed", "--", "run"],
        vec!["--unverified", "resource", "fixed", "--", "repo-lock-exec"],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--",
            "repo-prune-alias",
        ],
        vec!["--unverified", "prune", "--", "repo-prune"],
        vec![
            "--unverified",
            "--config",
            "resource",
            "fixed",
            "--",
            "repo-prune",
        ],
        vec!["--unverified", "resource", "fixed", "--cpus", "repo-prune"],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--cpus",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--cpus=0",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--cpus=4294967296",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--memory-mib=-1",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--exclusive=true",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--exclusive",
            "--exclusive",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--unknown",
            "--",
            "repo-prune",
        ],
        vec!["--unverified", "resource", "fixed", "repo-prune"],
        vec!["--unverified", "resource", "fixed", "--"],
        vec![
            "--unverified",
            "resource",
            "fixed",
            "--",
            "--",
            "repo-prune",
        ],
        vec![
            "--unverified",
            "--",
            "resource",
            "fixed",
            "--",
            "repo-prune",
        ],
        vec![
            "+trust",
            "--unverified",
            "resource",
            "fixed",
            "--",
            "repo-prune",
        ],
    ] {
        assert!(!command(&words(&args), Tool::Targo, "-"), "{args:?}");
    }
}

#[test]
fn unavailable_or_non_native_probe_responses_are_not_exemptions() {
    let native = b"{\"version\":1,\"fixed_admission\":\"original-fixed-v1\"}\n";
    let output = |status, stdout: &[u8], stderr: &[u8]| std::process::Output {
        status: std::process::ExitStatus::from_raw(status),
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    };
    assert!(answer(&output(0, native, b"")));
    assert!(!answer(&output(256, native, b"")));
    assert!(!answer(&output(9, native, b"")));
    assert!(!answer(&output(0, native, b"plugin warning")));
    for text in [
        b"".as_slice(),
        b"{\"version\":1,\"fixed_admission\":null}\n",
        b"{\"version\":3,\"cpu_workers\":true}\n",
        b"targo 0.14.0\n",
    ] {
        assert!(!answer(&output(0, text, b"")));
    }
}

#[test]
fn unmanaged_executable_cannot_supply_native_maintenance_authority() {
    let args = words(&[
        "--unverified",
        "resource",
        "fixed",
        "--",
        "repo-prune",
        "--dry-run",
    ]);
    // Even an actual executable cannot become a signed native frontend from
    // the caller's argv. The selected managed package must be authenticated.
    assert!(run(Path::new("/usr/bin/true"), Tool::Targo, "-", &args).is_none());
    assert!(
        run(
            Path::new("/does-not-exist/bin/targo"),
            Tool::Targo,
            "-",
            &args
        )
        .is_none()
    );
}
