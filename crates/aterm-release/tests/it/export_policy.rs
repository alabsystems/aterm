// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests `publish/manifest.txt` path selection and `publish/config.sh` public-clone check.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn git(args: &[&str]) -> String {
    let o = crate::ledger::testing::git()
        .arg("-C")
        .arg(repo())
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn specs(manifest: &str) -> Vec<String> {
    let denied = |p: &str| {
        ["publish", "docs", ".github"]
            .iter()
            .any(|d| p == *d || p.starts_with(&format!("{d}/")))
    };
    let mut specs: Vec<String> = manifest
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .filter_map(|l| match l.strip_prefix('!') {
            Some(body) => {
                let body = body.trim_start();
                Some(format!(
                    ":(exclude){}",
                    body.strip_prefix("./").unwrap_or(body)
                ))
            }
            None => {
                let row = l.strip_prefix("./").unwrap_or(l);
                (!denied(row)).then(|| row.to_string())
            }
        })
        .collect();
    specs.extend([":(exclude)publish", ":(exclude)docs", ":(exclude).github"].map(String::from));
    specs
}

#[test]
fn the_export_selects_no_gitlink_and_none_of_the_withheld_files() {
    let manifest = std::fs::read_to_string(repo().join("publish/manifest.txt")).unwrap();
    let specs = specs(&manifest);
    let mut args = vec!["ls-files", "-s", "--"];
    args.extend(specs.iter().map(String::as_str));
    let staged = git(&args);
    let gitlinks: Vec<&str> = staged
        .lines()
        .filter(|l| l.starts_with("160000 "))
        .collect();
    assert!(
        gitlinks.is_empty(),
        "the manifest selects gitlinks: {gitlinks:?}"
    );
    let exported: BTreeSet<String> = staged
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(_, p)| p.to_string()))
        .collect();
    assert!(!exported.is_empty());
    for withheld in [
        "publish/manifest.txt",
        "crates/aterm-release/tests/changelog.rs",
    ] {
        assert!(!exported.contains(withheld), "{withheld} is exported");
    }
    assert!(
        exported.contains("man/aterm.1"),
        "man/aterm.1 is not exported"
    );

    let art = "crates/aterm-effects/art";
    let tracked: BTreeSet<String> = git(&["ls-files", "--", art])
        .lines()
        .map(str::to_string)
        .collect();
    let mut expected: BTreeSet<String> = [
        "glyphs/README.md",
        "pet/README.md",
        "animal/README.md",
        "dogs/README.md",
    ]
    .iter()
    .map(|p| format!("{art}/{p}"))
    .collect();
    expected.extend((0..=22).map(|n| format!("{art}/glyphs/s2_{n:02}.toml")));
    for a in [
        "fish",
        "fishbone",
        "heart",
        "moon",
        "paw_black",
        "paw_pink",
        "paw_raised",
        "sparkle_dark",
        "sparkle_gold",
        "sparkle_pink",
        "star",
        "star_tiny",
    ] {
        expected.insert(format!("{art}/glyphs/acc_{a}.toml"));
    }
    assert_eq!(expected.len(), 39);
    let untracked: Vec<&String> = expected.iter().filter(|p| !tracked.contains(*p)).collect();
    assert!(
        untracked.is_empty(),
        "an exclusion names nothing tracked: {untracked:?}"
    );
    let left_out: BTreeSet<String> = tracked.difference(&exported).cloned().collect();
    assert_eq!(
        left_out, expected,
        "the export leaves out a different set of art files"
    );
    for kept in [
        "s2_06b.toml",
        "acc_bell.toml",
        "acc_bow.toml",
        "acc_crown.toml",
        "s1_00.toml",
        "spec_fluffy.toml",
    ] {
        assert!(
            exported.contains(&format!("{art}/glyphs/{kept}")),
            "roster asset {kept} ships"
        );
    }
}

#[test]
fn the_public_clone_command_reads_as_one_line_and_admits_only_a_public_version() {
    let config = std::fs::read_to_string(repo().join("publish/config.sh")).unwrap();
    let rows: Vec<&str> = config
        .lines()
        .filter(|l| l.starts_with("CHECK_CMD_DEFAULT="))
        .collect();
    let [row] = rows.as_slice() else {
        panic!("CHECK_CMD_DEFAULT is not one line: {rows:?}");
    };
    let cmd = row
        .strip_prefix("CHECK_CMD_DEFAULT=\"")
        .and_then(|r| r.trim_end().strip_suffix('"'))
        .filter(|c| !c.contains('"'))
        .unwrap_or_else(|| panic!("the engine cannot read {row}"));
    let clause = cmd
        .split(" && ")
        .find(|c| c.contains("aterm --version"))
        .expect("a version clause");
    let clause = clause
        .strip_prefix("sh -c '")
        .and_then(|c| c.strip_suffix('\''))
        .expect("the version clause is one sh -c step");
    let scratch = aterm_tempfile::tempdir().unwrap();
    let target = scratch.path().join("target with spaces [*]");
    let executable = target.join("debug/aterm");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' \"$ATERM_TEST_VERSION\"\nexit \"$ATERM_TEST_EXIT\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let run = |line: &str, exit: u8| {
        let mut command = aterm_uds::spawnfd::command("sh");
        command
            .args(["-c", clause])
            .current_dir(scratch.path())
            .env("CARGO_TARGET_DIR", &target)
            .env("ATERM_TEST_VERSION", line)
            .env("ATERM_TEST_EXIT", exit.to_string());
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap()
    };
    let toml = std::fs::read_to_string(repo().join("Cargo.toml")).unwrap();
    let workspace = toml
        .lines()
        .skip_while(|l| *l != "[workspace.package]")
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap();
    for good in [
        format!("aterm {workspace}"),
        "aterm 0.92.0".into(),
        "aterm 1.0.0".into(),
        "aterm 12.340.0".into(),
    ] {
        assert!(
            run(&good, 0).status.success(),
            "refuses a public version: {good:?}"
        );
    }
    assert!(
        run("aterm 0.92.0\nby ALab\nbuild: 42", 0).status.success(),
        "the normal multiline version answer remains public"
    );
    assert_eq!(
        run("aterm 0.92.0", 7).status.code(),
        Some(7),
        "a failed version command cannot pass through grep's successful match"
    );
    for bad in [
        "aterm 0.92.1",
        "aterm 0.92.10",
        "aterm 0.92.0+g1a2b3c4d",
        "aterm 0.92",
        "aterm 0.92.0-dev.1",
        "xaterm 0.92.0",
        "",
    ] {
        assert!(!run(bad, 0).status.success(), "admits {bad:?}");
    }
}
