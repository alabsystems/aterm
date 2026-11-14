// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn parse_reads_each_verb() {
    assert_eq!(parse(&[]), Ok(Verb::Help));
    assert_eq!(parse(&args(&["help"])), Ok(Verb::Help));
    assert_eq!(
        parse(&args(&[
            "smoke",
            "dist",
            "--keep",
            "--scratch",
            "s",
            "--frame",
            "f.png"
        ])),
        Ok(Verb::Smoke(smoke::Options {
            dist: PathBuf::from("dist"),
            scratch: Some(PathBuf::from("s")),
            keep: true,
            frame: Some(PathBuf::from("f.png")),
        }))
    );
    assert_eq!(
        parse(&args(&["update", "--no-install", "--repo", "r", "--log"])),
        Ok(Verb::Update {
            opts: update::Options {
                repo: Some(PathBuf::from("r")),
                no_install: true,
                restart: false,
                force: false
            },
            log: true,
        })
    );
    assert_eq!(
        parse(&args(&["update"])),
        Ok(Verb::Update {
            opts: update::Options {
                repo: None,
                no_install: false,
                restart: false,
                force: false
            },
            log: false,
        })
    );
    assert_eq!(
        parse(&args(&["schedule", "--daily", "03:15", "--dry-run"])),
        Ok(Verb::Schedule {
            at: "03:15".to_string(),
            repo: None,
            remove: false,
            dry_run: true
        })
    );
    assert_eq!(
        parse(&args(&["schedule", "--remove"])),
        Ok(Verb::Schedule {
            at: schedule::DEFAULT_TIME.to_string(),
            repo: None,
            remove: true,
            dry_run: false
        })
    );
    assert_eq!(
        parse(&args(&["status", "--repo", "r"])),
        Ok(Verb::Status {
            repo: Some(PathBuf::from("r"))
        })
    );
    // No --repo compiles nothing in: the repository is decided when the verb runs.
    assert_eq!(parse(&args(&["status"])), Ok(Verb::Status { repo: None }));
    assert_eq!(
        parse(&args(&["schedule", "--repo", "r"])),
        Ok(Verb::Schedule {
            at: schedule::DEFAULT_TIME.to_string(),
            repo: Some(PathBuf::from("r")),
            remove: false,
            dry_run: false
        })
    );
}

#[test]
fn parse_refuses_what_it_does_not_know() {
    for bad in [
        &["smoke"][..],
        &["smoke", "a", "b"],
        &["smoke", "a", "--scratch"],
        &["smoke", "a", "--restart"],
        &["update", "--staged"],
        &["update", "--no-install", "--restart"],
        &["schedule", "--daily", "25:00"],
        &["schedule", "--daily"],
        &["status", "extra"],
        &["install"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?} was taken");
    }
}

#[test]
fn a_usage_error_exits_2_and_help_exits_0() {
    assert_eq!(run(&args(&["install"])), USAGE);
    assert_eq!(run(&args(&["help"])), PASS);
}

#[test]
fn parse_reads_release_build() {
    let commit = "1a37de31a3401234567890abcdef1234567890ab";
    assert_eq!(
        parse(&args(&[
            "release-build",
            "--version",
            "0.200.0",
            "--build-number",
            "1794421328",
            "--commit",
            commit,
            "--out",
            "carry",
            "--repo",
            "r",
        ])),
        Ok(Verb::ReleaseBuild(release::Options {
            version: "0.200.0".to_string(),
            build: 1_794_421_328,
            commit: commit.to_string(),
            out: PathBuf::from("carry"),
            repo: Some(PathBuf::from("r")),
        }))
    );
    let full = [
        "release-build",
        "--version",
        "0.200.0",
        "--build-number",
        "1794421328",
        "--commit",
        commit,
        "--out",
        "carry",
    ];
    assert!(parse(&args(&full)).is_ok());
    // Each required flag left out, a value that is not one, and a word it does not know.
    for drop in [1, 3, 5, 7] {
        let mut less = full.to_vec();
        less.drain(drop..drop + 2);
        assert!(parse(&args(&less)).is_err(), "{less:?} was taken");
    }
    for (at, bad) in [(4, "12x"), (4, "0"), (6, "1a37de31a340"), (2, "v0.200.0")] {
        let mut wrong = full.to_vec();
        wrong[at] = bad;
        assert!(parse(&args(&wrong)).is_err(), "{wrong:?} was taken");
    }
    let mut extra = full.to_vec();
    extra.push("--sign");
    assert!(parse(&args(&extra)).is_err());
}
