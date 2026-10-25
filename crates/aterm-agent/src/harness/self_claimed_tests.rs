// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const UUID: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";

fn record(argv: &[&str], env: &[(&str, &str)]) -> Record {
    Record {
        pid: 4242,
        session_id: Some(UUID.to_string()),
        resume_argv: Some(argv.iter().map(|s| (*s).to_string()).collect()),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        status: Some("busy".to_string()),
        exit: None,
        cwd: Some("/work".to_string()),
        draft_empty: None,
        frame: None,
        initializing: false,
    }
}

#[cfg(unix)]
fn live() -> Live {
    Live {
        exec_path: "/store/codex-alab/bin/codex".to_string(),
        image: Some(PathBuf::from("/store/codex-alab/bin/codex")),
        argv: [
            "codex",
            "--search",
            "-m",
            "gpt-5",
            "-s",
            "read-only",
            "-i",
            "a.png",
            "fix the build",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect(),
        codex_home: Some("/Users//u/.codex".to_string()),
        cwd: Some(PathBuf::from("/work")),
        twin: Some(PathBuf::from("/Users//u/.aterm/agents/codex")),
        managed_launch: None,
    }
}

/// The live process's root flags, as a record carries them.
#[cfg(unix)]
const ROOT: [&str; 5] = ["--search", "-m", "gpt-5", "-s", "read-only"];

/// A record's line: `launch`, then `flags`, then `resume <UUID>`.
#[cfg(unix)]
fn line<'a>(launch: &'a str, flags: &[&'a str]) -> Vec<&'a str> {
    std::iter::once(launch)
        .chain(flags.iter().copied())
        .chain(["resume", UUID])
        .collect()
}

#[test]
fn a_record_parses_as_schema_v1_and_refuses_anything_else() {
    let text = format!(
        r#"{{"v":1,"pid":4242,"procStart":"Thu Oct 16 08:00:00 2026","sessionId":"{UUID}",
        "cwd":"/work","resumeArgv":["/a/codex","--yolo","resume","{UUID}"],
        "env":{{"CODEX_HOME":"/h"}},"status":"idle","exit":null,"claim":null}}"#
    );
    let r = parse(&text).expect("a v1 record");
    assert_eq!(r.pid, 4242);
    assert_eq!(r.session_id.as_deref(), Some(UUID));
    assert_eq!(r.resume_argv.as_ref().map(Vec::len), Some(4));
    assert_eq!(r.env, [("CODEX_HOME".to_string(), "/h".to_string())]);
    assert_eq!(r.exit, None);
    assert_eq!(parse(r#"{"v":2,"pid":1}"#), Err("version"));
    assert_eq!(parse(r#"{"v":1}"#), Err("pid"));
    assert_eq!(
        parse(r#"{"v":1,"pid":1,"resumeArgv":"x"}"#),
        Err("resumeArgv")
    );
    assert_eq!(
        parse(r#"{"v":1,"pid":1,"resumeArgv":[1]}"#),
        Err("resumeArgv")
    );
    assert_eq!(parse(r#"{"v":1,"pid":1,"env":{"A":1}}"#), Err("env"));
    assert_eq!(parse(r#"{"v":1,"pid":1,"sessionId":7}"#), Err("sessionId"));
    assert_eq!(parse("nope"), Err("not-json"));
}

#[test]
fn a_uuid_is_its_one_textual_form() {
    assert!(is_uuid(UUID));
    for bad in [
        "",
        "0199f2a4-6c1e-7a52-9d3b",
        "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b7",
        "0199f2a46c1e7a529d3b2f6c8e1a4b77",
        "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b7z",
        "--yolo",
    ] {
        assert!(!is_uuid(bad), "{bad:?}");
    }
}

/// A real producer's missing stable entry makes its shim exec physical A, but Codex preserves the
/// producer-bound route in its session record.
#[cfg(unix)]
#[test]
fn a_raw_fallback_record_keeps_its_producer_bound_resume_route() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = aterm_tempfile::Builder::new()
        .prefix("atpkg-raw-resume-")
        .tempdir()
        .expect("private producer prefix");
    let layout = atpkg::store::Layout {
        prefix: temp.path().canonicalize().unwrap(),
    };
    let package = layout.prefix.join("store/codex/2026101601");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    let physical = package.join("bin/codex");
    std::fs::write(&physical, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&physical, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        package.join("codex-package.json"),
        r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"A"}"#,
    )
    .unwrap();
    std::fs::write(package.join("alab-build.json"), "{}").unwrap();
    atpkg::store::mark_build_ready(&package).unwrap();
    atpkg::activate::activate_build(&layout, &package).unwrap();
    atpkg::activate::install_shims(
        &layout,
        &package,
        &["codex".to_owned()],
        atpkg::Aliases::Off,
    )
    .unwrap();
    let entry = layout.prefix.join("store/codex/exec/codex");
    assert_eq!(std::fs::read_link(&entry).unwrap(), physical);
    std::fs::remove_file(&entry).unwrap();
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: physical.to_str().unwrap().to_owned(),
        argv: [
            physical.to_str().unwrap(),
            "--search",
            "-m",
            "gpt-5",
            "-s",
            "read-only",
        ]
        .map(str::to_owned)
        .to_vec(),
        env: vec![
            "ATPKG_MANAGED_LAUNCH_PID=4242".to_owned(),
            format!("ATPKG_MANAGED_LAUNCH_PATH={}", entry.display()),
            format!("ATPKG_MANAGED_LAUNCH_TARGET={}", physical.display()),
        ],
    };
    let run = Live {
        exec_path: args.exec_path.clone(),
        image: Some(physical),
        argv: args.argv.clone(),
        twin: Some(layout.prefix.join("agents/codex")),
        managed_launch: atpkg::managed_launch::Hints::capture(&layout, &args),
        ..live()
    };
    assert!(run.managed_launch.is_some());
    assert!(
        !entry.exists(),
        "the producer's physical fallback was necessary"
    );
    let good = record(&line(entry.to_str().unwrap(), &ROOT), &[]);
    assert_eq!(
        validate(&good, 4242, &run).map(|resume| resume.argv),
        good.resume_argv.clone().ok_or("no-resume-line"),
    );
}

/// THE VALIDATION.
#[cfg(unix)]
#[test]
fn a_resume_line_validates_only_as_the_live_process_ran() {
    let home = [("CODEX_HOME", "/Users//u/.codex")];
    let twin = "/Users//u/.aterm/agents/codex";
    let good = record(&line(twin, &ROOT), &home);
    let resume = validate(&good, 4242, &live()).expect("its own line");
    assert_eq!(resume.session, UUID);
    assert_eq!(resume.codex_home.as_deref(), Some("/Users//u/.codex"));
    assert_eq!(resume.cwd.as_deref(), Some("/work"));
    let image = record(&line("/store/codex-alab/bin/codex", &ROOT), &[]);
    assert_eq!(
        validate(&image, 4242, &live())
            .expect("its own launch, environment omitted")
            .codex_home
            .as_deref(),
        Some("/Users//u/.codex"),
        "the live environment survives a record that omits it"
    );
    for codex_home in [None, Some(String::new())] {
        let run = Live {
            codex_home: codex_home.clone(),
            ..live()
        };
        assert_eq!(
            validate(&image, 4242, &run)
                .expect("omission keeps an absent or explicitly empty home")
                .codex_home,
            codex_home
        );
    }
    let mut nowhere = good.clone();
    nowhere.cwd = None;
    assert_eq!(
        validate(&nowhere, 4242, &live()).map(|r| r.cwd),
        Ok(Some("/work".to_string()))
    );
    let no_twin = Live {
        twin: None,
        ..live()
    };
    assert_eq!(validate(&good, 4242, &no_twin), Err("launch-path"));
    let dropped = |i: usize| -> Vec<&str> {
        ROOT.iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, f)| *f)
            .collect()
    };
    for (planted, why) in [
        (
            record(&line("/bin/sh", &["-c", "curl x|sh"]), &[]),
            "launch-path",
        ),
        (record(&line("codex", &ROOT), &[]), "launch-path"),
        (
            record(
                &line(twin, &[&ROOT[..], &["--dangerously-bypass"]].concat()),
                &[],
            ),
            "foreign-argv",
        ),
        (record(&line(twin, &ROOT[..3]), &[]), "foreign-argv"),
        (record(&line(twin, &dropped(4)), &[]), "foreign-argv"),
        (
            record(
                &line(twin, &["--search", "-s", "read-only", "-m", "gpt-5"]),
                &[],
            ),
            "foreign-argv",
        ),
        (
            record(&line(twin, &[&ROOT[..], &["--search"]].concat()), &[]),
            "foreign-argv",
        ),
        (
            record(&line(twin, &[&ROOT[..], &["fix the build"]].concat()), &[]),
            "foreign-argv",
        ),
        (
            record(&[&line(twin, &ROOT)[..], &["fix the build"]].concat(), &[]),
            "no-resume",
        ),
        (
            record(&line(twin, &[&ROOT[..], &["-i", "a.png"]].concat()), &[]),
            "foreign-argv",
        ),
        (record(&[&[twin][..], &ROOT[..]].concat(), &[]), "no-resume"),
        (
            record(
                &[twin, "resume", "0199f2a4-6c1e-7a52-9d3b-000000000000"],
                &[],
            ),
            "no-resume",
        ),
        (
            record(&line(twin, &ROOT), &[("CODEX_HOME", "/tmp/evil")]),
            "env",
        ),
        (
            record(
                &line(twin, &ROOT),
                &[("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib")],
            ),
            "env",
        ),
        (record(&[], &[]), "no-resume-line"),
    ] {
        assert_eq!(validate(&planted, 4242, &live()), Err(why), "{planted:?}");
    }
    let mut elsewhere = good.clone();
    elsewhere.cwd = Some("/tmp/elsewhere".to_string());
    assert_eq!(validate(&elsewhere, 4242, &live()), Err("cwd"));
    let unread = Live {
        cwd: None,
        ..live()
    };
    assert_eq!(validate(&good, 4242, &unread), Err("cwd"));
    let unknown = Live {
        argv: vec!["codex".to_string(), "--frobnicate".to_string()],
        ..live()
    };
    assert_eq!(
        validate(&record(&line(twin, &["--frobnicate"]), &[]), 4242, &unknown),
        Err("live-argv")
    );
    let mut not_uuid = good.clone();
    not_uuid.session_id = Some("abc".to_string());
    assert_eq!(validate(&not_uuid, 4242, &live()), Err("session-not-uuid"));
    assert_eq!(validate(&good, 4243, &live()), Err("pid"));
}

/// Codex's real resume writer keeps child options after `resume`. The window's
/// identity check must accept that command without moving either config scope.
#[cfg(unix)]
#[test]
fn a_resume_line_keeps_root_and_child_flags_in_their_launch_scopes() {
    let twin = "/Users//u/.aterm/agents/codex";
    let child = "--dangerously-bypass-approvals-and-sandbox";
    let make_live = |args: &[&str]| Live {
        argv: args.iter().map(|s| (*s).to_string()).collect(),
        ..live()
    };
    let launched = make_live(&["codex", "resume", child, UUID]);
    let actual = record(&[twin, "resume", child, UUID], &[]);
    assert_eq!(
        validate(&actual, 4242, &launched).map(|resume| resume.argv),
        actual.resume_argv.clone().ok_or("no-resume-line"),
        "the live producer's resume-with-child-option form"
    );

    let launched = make_live(&[
        "codex", "-c", "a=1", "fork", "--last", "-c", "a=2", "-m", "gpt-5",
    ]);
    let exact = [
        twin, "-c", "a=1", "resume", "-c", "a=2", "-m", "gpt-5", UUID,
    ];
    let good = record(&exact, &[]);
    assert!(
        validate(&good, 4242, &launched).is_ok(),
        "a fork resumes its own new root"
    );
    for changed in [
        // Same tokens and values, different root/child scopes.
        vec![
            twin, "resume", "-c", "a=1", "-c", "a=2", "-m", "gpt-5", UUID,
        ],
        vec![
            twin, "-c", "a=1", "-c", "a=2", "resume", "-m", "gpt-5", UUID,
        ],
        // A changed value, dropped override, or extra flag is no identity proof.
        vec![
            twin, "-c", "a=1", "resume", "-c", "a=3", "-m", "gpt-5", UUID,
        ],
        vec![twin, "-c", "a=1", "resume", "-m", "gpt-5", UUID],
        vec![
            twin, "-c", "a=1", "resume", "-c", "a=2", "-m", "gpt-5", child, UUID,
        ],
    ] {
        assert_eq!(
            validate(&record(&changed, &[]), 4242, &launched),
            Err("foreign-argv")
        );
    }
    let mut wrong_session = good.clone();
    *wrong_session
        .resume_argv
        .as_mut()
        .unwrap()
        .last_mut()
        .unwrap() = "0199f2a4-6c1e-7a52-9d3b-000000000000".to_string();
    assert_eq!(validate(&wrong_session, 4242, &launched), Err("no-resume"));
    let mut wrong_path = good.clone();
    wrong_path.resume_argv.as_mut().unwrap()[0] = "/other/codex".to_string();
    assert_eq!(validate(&wrong_path, 4242, &launched), Err("launch-path"));
    let mut wrong_cwd = good;
    wrong_cwd.cwd = Some("/other".to_string());
    assert_eq!(validate(&wrong_cwd, 4242, &launched), Err("cwd"));
}

#[test]
fn the_exit_is_decided_from_the_copy_alone() {
    let copy = |seen: Seen| Copy {
        pid: 4242,
        start: "s".to_string(),
        home: PathBuf::from("/h"),
        seen,
        resume: Err("no-record"),
    };
    let standing = |status: &str, exit: Option<&str>| {
        let mut r = record(&[], &[]);
        r.status = Some(status.to_string());
        r.exit = exit.map(str::to_owned);
        copy(Seen::Present(r))
    };
    assert_eq!(
        verdict(&copy(Seen::Removed), false),
        Verdict::Orderly("removed")
    );
    assert_eq!(
        verdict(&copy(Seen::Removed), true),
        Verdict::Orderly("removed")
    );
    assert_eq!(
        verdict(&copy(Seen::Unreadable("not-json")), false),
        Verdict::Orderly("not-json")
    );
    assert_eq!(verdict(&copy(Seen::Never), false), Verdict::Crash);
    assert!(!shows_end(&copy(Seen::Never)), "an agent that goes on");
    assert_eq!(verdict(&standing("busy", None), false), Verdict::Crash);
    assert_eq!(verdict(&standing("upgrading", None), false), Verdict::Crash);
    assert_eq!(
        verdict(&standing("upgrading", Some("exec-failed")), false),
        Verdict::Crash
    );
    assert_eq!(
        verdict(&standing("exited", None), false),
        Verdict::Orderly("exited")
    );
    assert_eq!(
        verdict(&standing("exited", Some("signal:15")), false),
        Verdict::Orderly("signal-not-aterms")
    );
    assert_eq!(
        verdict(&standing("exited", Some("signal:15")), true),
        Verdict::Crash
    );
}

#[test]
fn a_record_never_written_is_never_read_as_removed() {
    let copy = |pid: u32, start: &str, seen: Seen| Copy {
        pid,
        start: start.to_string(),
        home: PathBuf::from("/h"),
        seen,
        resume: Err("no-record-yet"),
    };
    let never = fold(None, copy(4242, "s", Seen::Never));
    assert_eq!(never.seen, Seen::Never);
    assert_eq!(verdict(&never, false), Verdict::Crash);
    let again = fold(Some(&never), copy(4242, "s", Seen::Never));
    assert_eq!(again.seen, Seen::Never, "two reads that found nothing");
    let standing = copy(4242, "s", Seen::Present(record(&[], &[])));
    let gone = fold(Some(&standing), copy(4242, "s", Seen::Never));
    assert_eq!(gone.seen, Seen::Removed);
    assert_eq!(gone.resume, Err("no-record"));
    assert_eq!(verdict(&gone, false), Verdict::Orderly("removed"));
    let unreadable = copy(4242, "s", Seen::Unreadable("not-json"));
    assert_eq!(
        fold(Some(&unreadable), copy(4242, "s", Seen::Never)).seen,
        Seen::Removed
    );
    let other = copy(4242, "t", Seen::Present(record(&[], &[])));
    assert_eq!(
        fold(Some(&other), copy(4242, "s", Seen::Never)).seen,
        Seen::Never
    );
}

/// THE LINE A NEVER-WRITTEN RECORD INHERITS ([`inherited`]).
#[cfg(unix)]
#[test]
fn a_never_written_record_inherits_only_the_line_its_relaunch_typed() {
    let mut relaunched = live();
    relaunched.argv = [
        "codex",
        "--search",
        "-m",
        "gpt-5",
        "-s",
        "read-only",
        "resume",
        UUID,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let typed = Resume {
        argv: line("/store/codex-alab/bin/codex", &ROOT)
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        session: UUID.to_string(),
        codex_home: Some("/Users//u/.codex".to_string()),
        cwd: Some("/work".to_string()),
    };
    let got = inherited(Some(&typed), 4242, &relaunched).expect("its own relaunch's line");
    assert_eq!(got.argv, typed.argv);
    assert_eq!(got.session, UUID);
    let made = copy_of(
        4242,
        "s",
        PathBuf::from("/h"),
        None,
        &relaunched,
        Some(&typed),
    );
    assert_eq!(made.seen, Seen::Never);
    assert_eq!(made.resume.as_ref().map(|r| r.session.as_str()), Ok(UUID));
    assert_eq!(inherited(None, 4242, &relaunched), Err("no-record-yet"));
    assert_eq!(
        copy_of(4242, "s", PathBuf::from("/h"), None, &relaunched, None).resume,
        Err("no-record-yet")
    );
    assert_eq!(
        inherited(Some(&typed), 4242, &live()),
        Err("not-the-typed-line")
    );
    let mut elsewhere = relaunched.clone();
    *elsewhere.argv.last_mut().expect("the id") =
        "0199f2a4-6c1e-7a52-9d3b-000000000000".to_string();
    assert_eq!(
        inherited(Some(&typed), 4242, &elsewhere),
        Err("not-the-typed-line")
    );
    let mut flags = relaunched.clone();
    flags.argv.retain(|a| a != "--search");
    assert_eq!(inherited(Some(&typed), 4242, &flags), Err("foreign-argv"));
    let mut path = typed.clone();
    path.argv[0] = "/elsewhere/codex".to_string();
    assert_eq!(
        inherited(Some(&path), 4242, &relaunched),
        Err("launch-path")
    );
}

/// The startup writer publishes its schema before the resumed native thread registers.
#[cfg(unix)]
#[test]
fn an_initial_record_inherits_only_its_proven_relaunch_line() {
    let mut relaunched = live();
    relaunched.argv = [
        "codex",
        "--search",
        "-m",
        "gpt-5",
        "-s",
        "read-only",
        "resume",
        UUID,
    ]
    .map(str::to_string)
    .to_vec();
    let typed = Resume {
        argv: line("/store/codex-alab/bin/codex", &ROOT)
            .into_iter()
            .map(str::to_string)
            .collect(),
        session: UUID.to_string(),
        codex_home: relaunched.codex_home.clone(),
        cwd: Some("/work".into()),
    };
    let initial = r#"{"v":1,"pid":4242,"sessionId":null,"resumeArgv":null,"cwd":null,"state":null,"status":"busy","exit":null,"draftEmpty":true,"env":{"CODEX_HOME":"/Users//u/.codex"}}"#;
    let make = |text: Option<&str>, live: &Live, typed: Option<&Resume>| {
        copy_of(4242, "start", "/h".into(), text, live, typed)
    };
    let copy = make(Some(initial), &relaunched, Some(&typed));
    assert!(matches!(copy.seen, Seen::Present(_)));
    assert_eq!(copy.resume, Ok(typed.clone()));
    assert_eq!(verdict(&copy, false), Verdict::Crash);
    assert_eq!(
        fold(Some(&copy), make(Some(initial), &relaunched, Some(&typed))).resume,
        Ok(typed.clone()),
        "another initial frame adds no authority"
    );
    assert!(make(Some(initial), &relaunched, None).resume.is_err());
    assert!(make(Some(initial), &live(), Some(&typed)).resume.is_err());
    let mut changed = relaunched.clone();
    changed.argv.retain(|arg| arg != "--search");
    assert!(make(Some(initial), &changed, Some(&typed)).resume.is_err());
    let mut foreign = relaunched.clone();
    *foreign.argv.last_mut().unwrap() = "0199f2a4-6c1e-7a52-9d3b-000000000000".into();
    assert!(make(Some(initial), &foreign, Some(&typed)).resume.is_err());
    let mut elsewhere = typed.clone();
    elsewhere.cwd = Some("/other".into());
    assert!(
        make(Some(initial), &relaunched, Some(&elsewhere))
            .resume
            .is_err()
    );
    elsewhere = typed.clone();
    elsewhere.argv[0] = "/other/codex".into();
    assert!(
        make(Some(initial), &relaunched, Some(&elsewhere))
            .resume
            .is_err()
    );

    for (from, to) in [
        ("\"pid\":4242", "\"pid\":4243"),
        ("\"sessionId\":null,", ""),
        ("\"resumeArgv\":null,", ""),
        ("\"cwd\":null,", ""),
        ("\"state\":null,", ""),
        ("\"exit\":null,", ""),
        ("\"draftEmpty\":true,", ""),
        (
            "\"sessionId\":null",
            "\"sessionId\":\"0199f2a4-6c1e-7a52-9d3b-000000000000\"",
        ),
        ("\"sessionId\":null", "\"sessionId\":7"),
        ("\"resumeArgv\":null", "\"resumeArgv\":[]"),
        (
            "\"resumeArgv\":null",
            "\"resumeArgv\":[\"/store/codex-alab/bin/codex\",\"resume\",\"0199f2a4-6c1e-7a52-9d3b-000000000000\"]",
        ),
        ("\"resumeArgv\":null", "\"resumeArgv\":7"),
        ("\"cwd\":null", "\"cwd\":\"/other\""),
        ("\"cwd\":null", "\"cwd\":7"),
        ("\"state\":null", "\"state\":\"working\""),
        ("\"state\":null", "\"state\":\"idle\""),
        ("\"state\":null", "\"state\":7"),
        ("\"status\":\"busy\"", "\"status\":\"idle\""),
        ("\"status\":\"busy\"", "\"status\":\"upgrading\""),
        ("\"exit\":null", "\"exit\":\"signal:15\""),
        ("\"draftEmpty\":true", "\"draftEmpty\":false"),
        ("\"draftEmpty\":true", "\"draftEmpty\":\"true\""),
        ("/Users//u/.codex", "/other/home"),
        ("\"CODEX_HOME\"", "\"OTHER_ENV\""),
        ("\"v\":1", "\"v\":2"),
    ] {
        let text = initial.replace(from, to);
        assert_ne!(text, initial, "the control changes {from}");
        assert!(
            make(Some(&text), &relaunched, Some(&typed)).resume.is_err(),
            "{text}"
        );
    }
    let asserted = initial.replace("\"state\":null,", "\"initializing\":true,");
    assert!(
        make(Some(&asserted), &relaunched, Some(&typed))
            .resume
            .is_err(),
        "a writer cannot assert the derived initialization fact"
    );

    let gone = fold(Some(&copy), make(None, &relaunched, Some(&typed)));
    assert_eq!(gone.seen, Seen::Removed, "initializers are real records");
    assert_eq!(verdict(&gone, false), Verdict::Orderly("removed"));
    let materialized = Copy {
        seen: Seen::Present(record(&line("/store/codex-alab/bin/codex", &ROOT), &[])),
        ..copy.clone()
    };
    let unreadable = Copy {
        seen: Seen::Unreadable("not-json"),
        ..copy.clone()
    };
    for before in [&gone, &materialized, &unreadable] {
        let refused = fold(Some(before), make(Some(initial), &relaunched, Some(&typed)));
        assert_eq!(refused.resume, Err("record-regressed"));
        assert_eq!(
            fold(
                Some(&refused),
                make(Some(initial), &relaunched, Some(&typed))
            )
            .resume,
            Err("record-regressed"),
            "another blank cannot revive the original launch"
        );
    }
    let other_identity = Copy {
        start: "earlier kernel start".into(),
        ..materialized
    };
    assert_eq!(
        fold(
            Some(&other_identity),
            make(Some(initial), &relaunched, Some(&typed))
        )
        .resume,
        Ok(typed),
        "another identity's observations do not taint this proven launch"
    );
}

#[test]
fn a_witness_is_handled_once() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let ledger = Witnesses::under(dir.path());
    let start = "Thu Oct 16 08:00:00 2026";
    assert!(!ledger.handled(4242, start));
    ledger.record(4242, start).expect("recorded");
    assert!(ledger.handled(4242, start));
    assert!(!ledger.handled(4242, "Thu Oct 16 08:00:01 2026"));
    assert!(!ledger.handled(4243, start));
    assert!(dir.path().join("drive").join(WITNESS_FILE).is_file());
}

#[test]
fn record_reads_are_bounded_regular_files_and_only_absence_is_never_seen() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let path = record_path(dir.path(), 4242);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
    assert_eq!(read_record(&path), Seen::Never);
    let mut text = r#"{"v":1,"pid":4242}"#.to_string();
    text.extend(std::iter::repeat_n(
        ' ',
        RECORD_MAX_BYTES as usize - text.len(),
    ));
    std::fs::write(&path, &text).expect("record at the byte bound");
    assert!(matches!(read_record(&path), Seen::Present(_)));
    text.push(' ');
    std::fs::write(&path, &text).expect("one byte too large");
    assert_eq!(read_record(&path), Seen::Unreadable("record-too-large"));
    std::fs::write(&path, [0xff]).expect("not UTF-8");
    assert_eq!(read_record(&path), Seen::Unreadable("record-unreadable"));
    std::fs::remove_file(&path).expect("remove record");
    std::fs::create_dir(&path).expect("directory in its place");
    assert_eq!(read_record(&path), Seen::Unreadable("record-unreadable"));
    std::fs::remove_dir(&path).expect("remove directory");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        // SAFETY: an owned NUL-terminated scratch path; no pointer is retained.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || tx.send(read_record(&path)).expect("receiver"));
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("record read hung on a FIFO without a writer"),
            Seen::Unreadable("record-unreadable")
        );
        reader.join().expect("reader");
    }
}

/// THE LIVE READ.
#[cfg(target_vendor = "apple")]
#[test]
fn the_record_is_read_only_while_its_process_runs() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )))
    .expect("the kept stand-in");
    let sleep_s = sleep.to_string_lossy().into_owned();
    let mut child = aterm_uds::spawnfd::command(&sleep)
        .arg("60")
        .env("CODEX_HOME", &home)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !atpkg::caller_shell::process_args(pid).is_some_and(|a| a.argv.first() == Some(&sleep_s))
    {
        assert!(std::time::Instant::now() < deadline, "sleep never ran");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let start = super::super::restart_stamp::kernel_start(pid).expect("its start");
    let path = record_path(&home, pid);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
    let home_s = home.to_string_lossy();
    std::fs::write(
        &path,
        format!(
            r#"{{"v":1,"pid":{pid},"sessionId":"{UUID}","resumeArgv":["{sleep_s}","resume","{UUID}"],"status":"busy","exit":null}}"#
        ),
    )
    .expect("the record");
    let copy = read_copy(pid, &start, None).expect("read while it runs");
    assert!(matches!(copy.seen, Seen::Present(_)), "{copy:?}");
    let resume = copy.resume.as_ref().expect("its own line validates");
    assert_eq!(resume.argv[0], sleep_s);
    assert_eq!(resume.codex_home.as_deref(), Some(home_s.as_ref()));
    assert_eq!(verdict(&copy, false), Verdict::Crash);
    std::fs::write(&path, vec![b' '; RECORD_MAX_BYTES as usize + 1]).expect("oversized record");
    let unreadable = read_copy(pid, &start, None).expect("live identity still vouched for");
    assert_eq!(unreadable.seen, Seen::Unreadable("record-too-large"));
    assert_eq!(unreadable.resume, Err("no-record"));
    std::fs::remove_file(&path).expect("removed");
    let copy = read_copy(pid, &start, None).expect("read while it runs");
    assert_eq!(
        copy.seen,
        Seen::Never,
        "one read alone cannot tell a removal"
    );
    let copy = fold(Some(&unreadable), copy);
    assert_eq!(
        copy.seen,
        Seen::Removed,
        "after a read that found it: removed"
    );
    assert_eq!(verdict(&copy, false), Verdict::Orderly("removed"));
    assert_eq!(read_copy(pid, "Thu Jan  1 00:00:00 1970", None), None);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(read_copy(pid, &start, None), None, "gone: nothing is read");
}

#[test]
fn the_idle_point_is_the_records_own_rest() {
    let parsed = |status: &str, draft: &str, exit: &str| {
        parse(&format!(
            r#"{{"v":1,"pid":4242,"status":{status},"draftEmpty":{draft},"exit":{exit}}}"#
        ))
        .expect("a v1 record")
    };
    assert!(idle_point(&parsed(r#""idle""#, "true", "null")));
    for (status, draft, exit) in [
        (r#""busy""#, "true", "null"),
        (r#""waiting""#, "true", "null"),
        (r#""upgrading""#, "true", "null"),
        (r#""exited""#, "true", "null"),
        ("null", "true", "null"),
        (r#""idle""#, "false", "null"),
        (r#""idle""#, "null", "null"),
        (r#""idle""#, r#""true""#, "null"),
        (r#""idle""#, "1", "null"),
        (r#""idle""#, "true", r#""signal:15""#),
    ] {
        let record = parsed(status, draft, exit);
        assert!(!idle_point(&record), "{status} {draft} {exit}: {record:?}");
    }
    let absent = parse(r#"{"v":1,"pid":4242,"status":"idle"}"#).expect("a v1 record");
    assert_eq!(absent.draft_empty, None);
    assert!(!idle_point(&absent), "no draft named is no idle point");
}

/// THE IDLE POINT IS READ LIVE.
#[cfg(target_vendor = "apple")]
#[test]
fn the_idle_point_is_read_live_from_the_record_of_a_running_process() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )))
    .expect("the kept stand-in");
    let sleep_s = sleep.to_string_lossy().into_owned();
    let mut child = aterm_uds::spawnfd::command(&sleep)
        .arg("60")
        .env("CODEX_HOME", &home)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !atpkg::caller_shell::process_args(pid).is_some_and(|a| a.argv.first() == Some(&sleep_s))
    {
        assert!(std::time::Instant::now() < deadline, "sleep never ran");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let start = super::super::restart_stamp::kernel_start(pid).expect("its start");
    let path = record_path(&home, pid);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
    let plant = |pid: u32, status: &str, draft: &str| {
        std::fs::write(
            &path,
            format!(
                r#"{{"v":1,"pid":{pid},"sessionId":"{UUID}","status":"{status}","draftEmpty":{draft},"exit":null}}"#
            ),
        )
        .expect("the record");
    };
    plant(pid, "idle", "true");
    assert!(read_idle(pid, &start), "its own record says it rests");
    plant(pid, "busy", "true");
    assert!(!read_idle(pid, &start), "busy");
    plant(pid, "idle", "false");
    assert!(!read_idle(pid, &start), "a draft in the composer");
    plant(pid, "idle", "null");
    assert!(!read_idle(pid, &start), "no draft named");
    plant(pid + 1, "idle", "true");
    assert!(!read_idle(pid, &start), "another pid's record");
    std::fs::write(&path, "not a record").expect("garbage");
    assert!(!read_idle(pid, &start), "unreadable");
    std::fs::remove_file(&path).expect("removed");
    assert!(!read_idle(pid, &start), "removed");
    plant(pid, "idle", "true");
    assert!(
        !read_idle(pid, "Thu Jan  1 00:00:00 1970"),
        "another identity of the same pid"
    );
    assert!(read_idle(pid, &start), "positive control, again");
    let plant_frame = |pid: u32, status: &str, state: &str| {
        std::fs::write(
            &path,
            format!(
                r#"{{"v":1,"pid":{pid},"sessionId":"{UUID}","status":"{status}","state":"{state}","draftEmpty":true,"exit":null}}"#
            ),
        )
        .expect("the record");
    };
    plant_frame(pid, "busy", "idle");
    assert_eq!(
        read_first_frame(pid, &start),
        Some(FirstFrame::Idle),
        "its frame at rest, a carry-on owed"
    );
    assert!(!read_idle(pid, &start), "which is no idle point");
    plant_frame(pid, "busy", "working");
    assert_eq!(
        read_first_frame(pid, &start),
        Some(FirstFrame::Turn),
        "a turn"
    );
    plant_frame(pid, "busy", "starting");
    assert_eq!(read_first_frame(pid, &start), None, "no frame this reads");
    plant_frame(pid + 1, "busy", "idle");
    assert_eq!(read_first_frame(pid, &start), None, "another pid's record");
    plant_frame(pid, "busy", "idle");
    assert_eq!(
        read_first_frame(pid, "Thu Jan  1 00:00:00 1970"),
        None,
        "another identity of the same pid"
    );
    assert_eq!(
        read_first_frame(pid, &start),
        Some(FirstFrame::Idle),
        "positive control, again"
    );
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !read_idle(pid, &start),
        "gone: a dead identity rests nowhere"
    );
    assert_eq!(
        read_first_frame(pid, &start),
        None,
        "gone: no first frame either"
    );
}

#[test]
fn a_relaunchs_first_frame_is_its_rest_or_a_turn_even_while_a_carry_on_is_owed() {
    let parsed = |status: &str, state: &str, exit: &str| {
        parse(&format!(
            r#"{{"v":1,"pid":4242,"status":{status},"state":{state},"draftEmpty":true,"exit":{exit}}}"#
        ))
        .expect("a v1 record")
    };
    let owed = parsed(r#""busy""#, r#""idle""#, "null");
    assert!(!idle_point(&owed), "a carry-on owed: no idle point");
    assert_eq!(
        first_frame(&owed),
        Some(FirstFrame::Idle),
        "but its first idle"
    );
    for state in [r#""done""#, r#""error""#] {
        assert_eq!(
            first_frame(&parsed(r#""busy""#, state, "null")),
            Some(FirstFrame::Idle),
            "{state}"
        );
    }
    assert_eq!(
        first_frame(&parsed(r#""idle""#, "null", "null")),
        Some(FirstFrame::Idle),
        "the record's own rest"
    );
    for (status, state) in [
        (r#""busy""#, r#""working""#),
        (r#""waiting""#, r#""blocked""#),
    ] {
        assert_eq!(
            first_frame(&parsed(status, state, "null")),
            Some(FirstFrame::Turn),
            "{status} {state}"
        );
    }
    for (status, state, exit) in [
        (r#""busy""#, "null", "null"),
        (r#""busy""#, "1", "null"),
        (r#""busy""#, r#""starting""#, "null"),
        (r#""busy""#, r#""idle""#, r#""signal:9""#),
        (r#""busy""#, r#""working""#, r#""signal:9""#),
        (r#""idle""#, r#""idle""#, r#""signal:15""#),
    ] {
        let record = parsed(status, state, exit);
        assert_eq!(
            first_frame(&record),
            None,
            "{status} {state} {exit}: {record:?}"
        );
    }
}

#[test]
fn a_proven_claim_names_its_codex_alab_only_when_proven() {
    assert_eq!(
        proven_pid("OK schema=1 hold=0 supervisor=codex-alab%404242 supervisor_proven=1 hand=-"),
        Some(4242)
    );
    assert_eq!(
        proven_pid("OK supervisor=codex-alab@4242 supervisor_proven=1"),
        Some(4242)
    );
    for status in [
        "OK supervisor=codex-alab@4242 supervisor_proven=0",
        "OK supervisor=codex-alab@4242",
        "OK supervisor=- supervisor_proven=1",
        "OK supervisor=aterm-harness%401 supervisor_proven=1",
        "OK supervisor=codex-alab@x supervisor_proven=1",
        "ERR no such session supervisor=codex-alab@4242 supervisor_proven=1",
    ] {
        assert_eq!(proven_pid(status), None, "{status}");
    }
}
