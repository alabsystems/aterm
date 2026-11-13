// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn input_check_requires_only_a_base_and_cannot_request_publication() {
    let args = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(matches!(parse(&args(&["input-check", "--base", "abc"])),
        Ok(Cmd::InputCheck(base)) if base == "abc"));
    for parts in [
        vec!["input-check"],
        vec!["input-check", "--base"],
        vec!["input-check", "--base", "abc", "--publish"],
    ] {
        assert!(parse(&args(&parts)).is_err());
    }
}

#[test]
fn qualify_replay_requires_historical_windows_and_independent_pilot_takes() {
    let args = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(
        matches!(parse(&args(&["qualify-replay","cuts","runs.json"])),Ok(Cmd::QualifyReplay(cuts,runs)) if cuts==std::path::Path::new("cuts") && runs==std::path::Path::new("runs.json"))
    );
    assert!(parse(&args(&["qualify-replay", "cuts"])).is_err());
    assert!(parse(&args(&["qualify-replay", "cuts", "runs.json", "extra"])).is_err());
}
#[test]
fn prefreeze_is_a_standalone_nonpublishing_operation() {
    let parse = |flags: &[&str]| parse_release(&mut flags.iter().copied());
    assert!(parse(&["--prefreeze"]).unwrap().prefreeze);
    assert!(!parse(&["--mac-only"]).unwrap().prefreeze);
    for flags in [
        vec!["--bump"],
        vec!["--commit", "HEAD"],
        vec!["--fix", "HEAD"],
        vec!["--prepare"],
        vec!["--qualify"],
        vec!["--forget"],
        vec!["--abandon", "v0.1.0"],
        vec!["--dry-run"],
        vec!["--mac-only"],
        vec!["--release-credentials", "/unused/profile.toml"],
    ] {
        let mut args = vec!["--prefreeze"];
        args.extend(flags);
        assert!(parse(&args).is_err(), "{args:?}");
    }
}
#[test]
fn supersede_requires_an_explicit_commit() {
    let parse = |words: &[&str]| {
        super::parse(
            &words
                .iter()
                .map(|word| (*word).to_string())
                .collect::<Vec<_>>(),
        )
    };
    assert!(parse(&["release", "--supersede"]).is_err());
    assert!(parse(&["release", "--commit", "abc", "--supersede"]).is_ok());
    assert!(parse(&["release", "--commit", "abc", "--supersede", "--qualify"]).is_err());
}
use super::*;

#[test]
fn compat_checks_only_the_stable_compile_without_exporting_or_cutting() {
    let args = ["check", "--compat"].map(str::to_owned);
    let Cmd::Check(opts) = parse(&args).unwrap() else {
        panic!("check verb");
    };
    assert!(opts.compat);
    assert_eq!(opts.sections(), (false, false, false));
    assert_eq!(
        crate::release_steps::CheckOptions::default().sections(),
        (true, true, true)
    );
    assert!(parse(&["check", "--source"].map(str::to_owned)).is_err());
    let Cmd::Check(source) = parse(&["check", "--cut", "--source"].map(str::to_owned)).unwrap()
    else {
        panic!("source check");
    };
    assert!(source.cut && source.source);
}

/// `inputs` takes a new seal, `inputs --recheck` rechecks the kept one, and any other
/// argument is refused by its own spelling, after `--recheck` too. CONTROL: the two forms.
#[test]
fn inputs_takes_only_recheck_and_names_the_argument_it_refuses() {
    let parse_args = |args: &[&str]| {
        let args: Vec<String> = std::iter::once("inputs")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        parse(&args)
    };
    assert!(matches!(
        parse_args(&[]).unwrap(),
        Cmd::Inputs { recheck: false }
    ));
    assert!(matches!(
        parse_args(&["--recheck"]).unwrap(),
        Cmd::Inputs { recheck: true }
    ));
    for (args, refused) in [
        (&["--fresh"][..], "\"--fresh\""),
        (&["--recheck", "extra"][..], "\"extra\""),
        (&["--recheck", "--recheck"][..], "\"--recheck\""),
    ] {
        let error = parse_args(args).expect_err("refused");
        assert!(
            error.contains(&format!("(got {refused})")),
            "{args:?}: {error}"
        );
    }
}

#[test]
fn notary_only_cannot_select_other_provisioning_work() {
    let parse_flags = |flags: &[&str]| {
        let args: Vec<String> = ["provision", "--notary-only"]
            .into_iter()
            .chain(flags.iter().copied())
            .map(str::to_string)
            .collect();
        parse(&args)
    };
    assert!(matches!(
        parse_flags(&[]).unwrap(),
        Cmd::NotaryOnly { check: false }
    ));
    assert!(matches!(
        parse_flags(&["--check"]).unwrap(),
        Cmd::NotaryOnly { check: true }
    ));
    for flags in [
        vec!["--id", "m22"],
        vec!["--roster-only"],
        vec!["--roster", "/public/pair"],
        vec!["--cert-dir", "/cert"],
        vec!["--notary-only"],
    ] {
        assert!(parse_flags(&flags).is_err());
    }
}

#[test]
fn roster_only_requires_an_explicit_pair_and_cannot_acquire_certificates() {
    let parse_flags = |flags: &[&str]| {
        let args: Vec<String> = ["provision", "--id", "m3"]
            .into_iter()
            .chain(flags.iter().copied())
            .map(str::to_string)
            .collect();
        parse(&args)
    };
    for flags in [
        vec!["--roster-only"],
        vec!["--roster", "/public/pair"],
        vec![
            "--roster-only",
            "--roster",
            "/public/pair",
            "--cert-dir",
            "/cert",
        ],
    ] {
        assert!(parse_flags(&flags).is_err());
    }
    assert!(
        matches!(parse_flags(&["--roster-only", "--roster", "/public/pair", "--check"]).unwrap(),
        Cmd::Provision { check: true, roster: Some(path), cert_dir: None, .. }
            if path == std::path::Path::new("/public/pair"))
    );
    assert!(matches!(
        parse_flags(&[]).unwrap(),
        Cmd::Provision { roster: None, .. }
    ));
}
