// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn native_selection_requires_matching_explicit_frontend_and_unique_flags() {
    let words = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
    let frontend = "a".repeat(64);
    let targo = "b".repeat(64);
    for args in [
        words(&[
            "--frontend-component",
            &frontend,
            "--targo-component",
            &targo,
            "pack",
        ]),
        words(&[
            "--targo-component",
            &targo,
            "--frontend-component",
            &frontend,
            "pack",
        ]),
    ] {
        let (f, t, rest) = super::component_flags(&args).unwrap();
        assert_eq!(f.as_deref(), Some(frontend.as_str()));
        assert_eq!(t.as_deref(), Some(targo.as_str()));
        assert_eq!(rest, &["pack"]);
    }
    for args in [
        words(&["--targo-component", &targo, "pack"]),
        words(&["--targo-component", "bad", "pack"]),
        words(&[
            "--frontend-component",
            &frontend,
            "--frontend-component",
            &frontend,
            "pack",
        ]),
        words(&[
            "--frontend-component",
            &frontend,
            "--targo-component",
            &targo,
            "--targo-component",
            &targo,
            "pack",
        ]),
        words(&[
            "--frontend-component",
            &frontend,
            "--targo-component",
            &targo,
            "index",
        ]),
    ] {
        assert!(matches!(
            super::run(&args),
            Err(crate::error::Error::Usage(_))
        ));
    }
}

#[test]
fn frontend_selection_is_explicit_and_rejects_unused_or_invalid_authority() {
    let words = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
    let sha = "a".repeat(64);
    let args = words(&[
        "--frontend-component",
        &sha,
        "publish-group",
        "rustc",
        "--dry-run",
    ]);
    let (selected, rest) = super::frontend_flag(&args).unwrap();
    assert_eq!(selected.as_deref(), Some(sha.as_str()));
    assert_eq!(rest, &args[2..]);
    let args = words(&["publish-group", "rustc"]);
    assert_eq!(
        super::frontend_flag(&args).unwrap(),
        (None, args.as_slice())
    );
    for args in [
        words(&["--frontend-component"]),
        words(&["--frontend-component", ""]),
        words(&["--frontend-component=not-a-hash", "pack"]),
        words(&["--frontend-component", &sha, "status"]),
        words(&["--frontend-component", &sha, "publish-group", "unrelated"]),
    ] {
        assert!(matches!(
            super::run(&args),
            Err(crate::error::Error::Usage(_))
        ));
    }
}

#[test]
fn identity_learning_is_only_for_an_explicit_publishing_run() {
    let words = |args: &[&str]| args.iter().map(ToString::to_string).collect::<Vec<_>>();
    for (verb, args) in [
        ("hosts", vec!["check"]),
        ("mirror", vec!["quad", "--dry-run"]),
        ("publish-group", vec!["rustc", "--dry-run"]),
        ("pack", vec!["ay", "--unsigned"]),
        ("index", vec![]),
        ("status", vec![]),
    ] {
        assert!(!super::live_identity_lookup(verb, &words(&args)));
    }
    assert!(super::live_identity_lookup(
        "upload",
        &words(&["staging", "repo"])
    ));
    assert!(super::live_identity_lookup("index", &words(&["--upload"])));
}

use std::ffi::OsString;

use super::{forwarded, host_flag, run, utf8_words};
use crate::ctx::HostChoice;
use crate::error::Error;

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

#[test]
fn host_flag_strips_only_a_leading_choice() {
    let args = words(&["--host", "agh", "lane", "run", "linux"]);
    let (choice, rest) = host_flag(&args).expect("a leading choice");
    assert_eq!(choice, Some(HostChoice::Agh));
    assert_eq!(rest, words(&["lane", "run", "linux"]).as_slice());

    let args = words(&["--host=agh-staging", "status"]);
    let (choice, rest) = host_flag(&args).expect("a leading choice");
    assert_eq!(choice, Some(HostChoice::AghStaging));
    assert_eq!(rest, words(&["status"]).as_slice());

    let args = words(&["status", "--host", "agh"]);
    let (choice, rest) = host_flag(&args).expect("no leading choice");
    assert_eq!(choice, None);
    assert_eq!(rest, args.as_slice());

    assert!(matches!(
        host_flag(&words(&["--host"])),
        Err(Error::Usage(_))
    ));
    match host_flag(&words(&["--host", "github"])) {
        Err(Error::Usage(why)) => assert!(why.contains("github"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn local_host_selection_reaches_the_actual_context_factory_before_metadata() {
    for (args, expected) in [
        (
            vec![
                "hosts",
                "check",
                "--host",
                "agh",
                "--unit-env",
                "--lane",
                "linux",
            ],
            HostChoice::Agh,
        ),
        (
            vec!["hosts", "check", "--host=agh-staging", "--lane", "linux"],
            HostChoice::AghStaging,
        ),
        (vec!["hosts", "check", "--host", "agh"], HostChoice::Gh),
        (
            vec!["lane", "install", "linux", "--host", "agh", "--no-arm"],
            HostChoice::Agh,
        ),
        (vec!["lane", "install", "linux"], HostChoice::Gh),
    ] {
        let observed = std::cell::Cell::new(None);
        let error = super::run_with_context(&words(&args), |choice, learn| {
            observed.set(Some((choice, learn)));
            Err(Error::cannot_run("owned context factory observation"))
        })
        .expect_err("the inert factory stops before any host request");
        assert_eq!(observed.get(), Some((expected, false)), "{args:?}: {error}");
    }
}

#[test]
fn invalid_local_host_flags_and_global_conflicts_construct_no_context() {
    for args in [
        vec!["hosts", "check", "--host", "bad", "--lane", "linux"],
        vec!["hosts", "check", "--lane"],
        vec!["lane", "install", "linux", "--host", "bad"],
        vec!["--host", "agh", "lane", "install", "linux"],
    ] {
        let called = std::cell::Cell::new(false);
        let error = super::run_with_context(&words(&args), |_, _| {
            called.set(true);
            Err(Error::cannot_run("unexpected context construction"))
        })
        .expect_err("invalid flags refuse before identity lookup");
        assert!(matches!(error, Error::Usage(_)), "{args:?}: {error}");
        assert!(!called.get(), "{args:?}");
    }
}

#[test]
fn forwarded_takes_a_leading_agh_alone() {
    let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        forwarded(&words(&["agh", "auth", "git-credential", "get"])),
        Some(os(&["auth", "git-credential", "get"]))
    );
    assert_eq!(forwarded(&words(&["agh"])), Some(Vec::new()));
    assert_eq!(forwarded(&words(&["lane", "run"])), None);
    assert_eq!(forwarded(&words(&["--host", "agh", "agh"])), None);
}

#[test]
fn run_refuses_a_global_host_before_hosts_and_agh() {
    for args in [
        &["--host", "agh", "hosts", "check"][..],
        &["--host", "agh", "agh", "api"][..],
        &["--host", "agh-staging", "lane", "install", "alab"][..],
    ] {
        let e = run(&words(args)).expect_err("a global --host before hosts, agh or install");
        assert!(matches!(e, Error::Usage(_)), "{args:?}: {e}");
        assert_eq!(e.exit_code(), 2, "{args:?}: {e}");
    }
}

/// A non-UTF-8 argument is Usage before any verb runs, and one after a leading `agh` is
/// handed to agh's CLI as the OS gave it.
#[cfg(unix)]
#[test]
fn a_non_utf8_argument_is_usage_and_agh_gets_it_whole() {
    use std::os::unix::ffi::OsStringExt as _;

    let bad = OsString::from_vec(b"/repos/\xff".to_vec());
    match utf8_words(vec![OsString::from("status"), bad.clone()]) {
        Err(Error::Usage(why)) => assert_eq!(why, "argument 2 is not UTF-8"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        utf8_words(vec![OsString::from("status")]).expect("UTF-8"),
        words(&["status"])
    );
    assert_eq!(
        forwarded(&[OsString::from("agh"), OsString::from("api"), bad.clone()]),
        Some(vec![OsString::from("api"), bad])
    );
}
