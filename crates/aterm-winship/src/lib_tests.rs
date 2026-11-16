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
            sign: None,
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

#[test]
fn parse_reads_the_pull_lane_verbs() {
    let commit = "1a37de31a3401234567890abcdef1234567890ab";
    let build = [
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
    let mut signed = build.to_vec();
    signed.extend(["--sign-with", "k.pk8", "--builder", "m6"]);
    let Ok(Verb::ReleaseBuild(opts)) = parse(&args(&signed)) else {
        panic!("{signed:?} was not read");
    };
    assert_eq!(
        opts.sign,
        Some(release::Sign {
            key: PathBuf::from("k.pk8"),
            builder: Some("m6".to_string()),
        })
    );
    let mut keyed = build.to_vec();
    keyed.extend(["--sign-with", "k.pk8"]);
    let Ok(Verb::ReleaseBuild(opts)) = parse(&args(&keyed)) else {
        panic!("{keyed:?} was not read");
    };
    assert_eq!(opts.sign.and_then(|s| s.builder), None);
    assert_eq!(
        parse(&args(&[
            "release-key",
            "init",
            "--key",
            "k",
            "--builder",
            "m6"
        ])),
        Ok(Verb::ReleaseKey {
            init: true,
            key: Some(PathBuf::from("k")),
            builder: Some("m6".to_string()),
        })
    );
    assert_eq!(
        parse(&args(&["release-key", "show"])),
        Ok(Verb::ReleaseKey {
            init: false,
            key: None,
            builder: None,
        })
    );
    assert_eq!(
        parse(&args(&[
            "release-serve",
            "--once",
            "--repo",
            "r",
            "--key",
            "k",
            "--builder",
            "m6",
            "--log"
        ])),
        Ok(Verb::ReleaseServe {
            opts: serve::Options {
                once: true,
                repo: Some(PathBuf::from("r")),
                key: Some(PathBuf::from("k")),
                builder: Some("m6".to_string()),
            },
            log: true,
        })
    );
    assert_eq!(
        parse(&args(&["release-serve"])),
        Ok(Verb::ReleaseServe {
            opts: serve::Options {
                once: false,
                repo: None,
                key: None,
                builder: None,
            },
            log: false,
        })
    );
    // Negative controls.
    let mut builder_alone = build.to_vec();
    builder_alone.extend(["--builder", "m6"]);
    let mut bad_builder = build.to_vec();
    bad_builder.extend(["--sign-with", "k", "--builder", "M6 HP"]);
    for bad in [
        builder_alone,
        bad_builder,
        vec!["release-key"],
        vec!["release-key", "make"],
        vec!["release-key", "init", "--key"],
        vec!["release-key", "show", "--builder", "-x"],
        vec!["release-serve", "--daily"],
        vec!["release-serve", "--builder", "A"],
    ] {
        assert!(parse(&args(&bad)).is_err(), "{bad:?} was taken");
    }
}

#[test]
fn release_key_prints_the_pin_and_never_the_private_key() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let path = scratch.path().join("ed25519.pk8");
    let mut lines = Vec::new();
    // `show` with no key made says how to make one.
    let code = release_key(false, &path, "m6", 1_794_424_462, &mut |l| {
        lines.push(l.to_string());
    });
    assert_eq!(code, NOT_RUN, "{lines:?}");
    assert!(lines.concat().contains("release-key init"), "{lines:?}");
    lines.clear();
    let code = release_key(true, &path, "m6", 1_794_424_462, &mut |l| {
        lines.push(l.to_string());
    });
    assert_eq!(code, PASS, "{lines:?}");
    let key = std::fs::read(&path).expect("key");
    let public = attest::public_key(&key).expect("public");
    let said = lines.join("\n");
    assert!(said.contains("release-key: made"), "{said}");
    assert!(
        said.contains(&format!("public_key = \"{public}\"")),
        "{said}"
    );
    assert!(said.contains("id = \"m6\""), "{said}");
    // Pinned for 90 days, not a year: a copied key attests no longer than that.
    assert!(said.contains("not_after = \"2027-02-09\""), "{said}");
    // The pin it prints reads as a pins file that admits this key.
    let pin: String = lines
        .iter()
        .skip_while(|l| *l != "[[builder]]")
        .map(|l| format!("{l}\n"))
        .collect();
    let pinned = attest::Builders::parse(&pin).expect("the printed pin reads");
    assert!(pinned.admits("m6", &public, "2026-11-15").is_ok());
    // No run of 16 bytes of the key file is printed but the public key's own.
    let document = aterm_codec::hex::encode_lower(&key);
    for at in (0..=document.len() - 32).step_by(2) {
        let run = &document[at..at + 32];
        if !public.contains(run) {
            assert!(
                !said.contains(run),
                "{run} of the key file was printed: {said}"
            );
        }
    }
    // A second init keeps the key it made.
    lines.clear();
    assert_eq!(
        release_key(true, &path, "m6", 1_794_424_462, &mut |l| lines
            .push(l.to_string())),
        PASS
    );
    assert!(lines.concat().contains("is kept"), "{lines:?}");
    assert_eq!(std::fs::read(&path).expect("key"), key);
}
