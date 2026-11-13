// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};

#[test]
fn an_inventory_that_cannot_be_read_is_unknown_and_a_missing_one_is_empty() {
    let paths = Paths {
        root: std::path::PathBuf::from("/ctl/machines"),
    };
    assert!(matches!(
        scan_inventory(&Fake::root(), &paths),
        Read::Absent
    ));
    let corrupt = Fake::root();
    corrupt.file(
        "/ctl/machines/machines.toml",
        b"schema = 1\n[[host]]\nname = 3\n",
    );
    assert!(matches!(
        scan_inventory(&corrupt, &paths),
        Read::Unusable(_)
    ));
    let fine = Fake::root();
    fine.file("/ctl/machines/machines.toml", b"schema = 1\n");
    assert!(matches!(scan_inventory(&fine, &paths), Read::Value(_)));
}

#[test]
fn the_lan_agent_earlier_builds_installed_is_unloaded_and_what_it_made_taken_away() {
    let home = Path::new("/Users//alice");
    let plist = "/Users//alice/Library/LaunchAgents/systems.alab.aterm-machines.plist";
    let dir = "/Users//alice/.local/share/aterm-machines";
    let bootout = "/bin/launchctl bootout gui/501/systems.alab.aterm-machines";
    let alice = || {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys
    };
    let real_dir = crate::port::Stat {
        uid: 501,
        gid: 20,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    };
    let installed = || {
        let sys = alice();
        sys.file(plist, b"<plist/>")
            .file(&format!("{dir}/.metadata_never_index"), b"")
            .file(&format!("{dir}/bin/aterm"), b"a copy of aterm")
            .file(&format!("{dir}/state/daemon.log"), b"refused: \n")
            .file(&format!("{dir}/state/daemon.sock"), b"")
            .file(&format!("{dir}/state/notes.txt"), b"not the job's")
            .on(bootout, Ran::ok(""));
        for parent in [
            home.to_path_buf(),
            home.join(".local"),
            home.join(".local/share"),
            home.join("Library"),
            home.join("Library/LaunchAgents"),
        ] {
            sys.stat_of(&parent.to_string_lossy(), real_dir);
        }
        for sub in ["", "/bin", "/state", "/state/runs"] {
            sys.stat_of(&format!("{dir}{sub}"), real_dir);
        }
        sys
    };
    let sys = installed();
    retire_agent(&sys, home);
    let calls = sys.calls();
    let at = |c: &str| calls.iter().position(|x| x == c);
    assert!(at(bootout).is_some(), "{calls:?}");
    assert!(at(bootout) < at(&format!("remove {plist}")), "{calls:?}");
    for gone in [
        plist.to_owned(),
        format!("{dir}/.metadata_never_index"),
        format!("{dir}/bin/aterm"),
        format!("{dir}/state/daemon.log"),
        format!("{dir}/state/daemon.sock"),
    ] {
        assert_eq!(sys.written(&gone), None, "{gone}");
    }
    let removed: Vec<&str> = calls
        .iter()
        .filter_map(|c| c.strip_prefix("remove "))
        .collect();
    assert_eq!(
        removed,
        [
            plist.to_owned(),
            format!("{dir}/.metadata_never_index"),
            format!("{dir}/bin/aterm"),
            format!("{dir}/state/daemon.log"),
            format!("{dir}/state/daemon.sock"),
            format!("{dir}/bin"),
            format!("{dir}/state/runs"),
            format!("{dir}/state"),
            dir.to_owned(),
        ]
    );
    assert!(sys.written(&format!("{dir}/state/notes.txt")).is_some());

    let sys = installed();
    let link = crate::port::Stat {
        dir: false,
        symlink: true,
        ..real_dir
    };
    sys.stat_of(&format!("{dir}/state"), link)
        .stat_of(&format!("{dir}/bin"), link)
        .file(&format!("{dir}/cache"), b"a file of mine");
    retire_agent(&sys, home);
    let calls = sys.calls();
    for kept in [
        "/state",
        "/state/runs",
        "/state/daemon.log",
        "/bin",
        "/bin/aterm",
        "/cache",
    ] {
        let kept = format!("remove {dir}{kept}");
        assert!(!calls.contains(&kept), "{kept}: {calls:?}");
    }
    assert!(calls.contains(&format!("remove {dir}/.metadata_never_index")));

    let sys = alice();
    retire_agent(&sys, home);
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    let sys = installed();
    sys.stat_of(
        dir,
        crate::port::Stat {
            dir: false,
            symlink: true,
            ..real_dir
        },
    );
    retire_agent(&sys, home);
    assert!(sys.written(&format!("{dir}/bin/aterm")).is_some());
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.starts_with(&format!("remove {dir}")))
    );
    let sys = Fake::root();
    sys.file(plist, b"<plist/>");
    retire_agent(&sys, home);
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}

#[test]
fn only_a_shell_setup_that_is_not_a_dry_run_retires_the_job() {
    let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    assert!(pkg_retires(&args(&["shell-setup"])));
    assert!(pkg_retires(&args(&["shell-setup", "--path"])));
    assert!(!pkg_retires(&args(&["shell-setup", "--dry-run"])));
    assert!(!pkg_retires(&args(&["shell-setup", "--path", "--dry-run"])));
    assert!(!pkg_retires(&args(&["install", "shell-setup"])));
    assert!(!pkg_retires(&[]));
}

#[test]
fn a_retired_directory_is_swept_only_where_the_job_made_it() {
    use std::os::unix::fs::symlink;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("at-retire-")
        .tempdir_in("/tmp")
        .unwrap();
    let t = tmp.path();
    let live = Live::new();
    let write = |p: &Path| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    };
    let there = |p: &Path| p.symlink_metadata().is_ok();

    let job = t.join("job");
    for d in ["bin", "state/runs", "cache", "stage"] {
        std::fs::create_dir_all(job.join(d)).unwrap();
    }
    for f in [
        ".metadata_never_index",
        "bin/aterm",
        "state/daemon.log",
        "stage/notes",
    ] {
        write(&job.join(f));
    }
    let _sock = std::os::unix::net::UnixListener::bind(job.join("state/daemon.sock")).unwrap();
    write(&t.join("mine/inbox/keep"));
    symlink(t.join("mine/inbox"), job.join("inbox")).unwrap();
    sweep_retired_dir(&live, t, Path::new("job"));
    for gone in [".metadata_never_index", "bin", "state", "cache"] {
        assert!(!there(&job.join(gone)), "{gone}");
    }
    for kept in ["stage/notes", "inbox", "inbox/keep"] {
        assert!(there(&job.join(kept)), "{kept}");
    }
    std::fs::remove_file(job.join("stage/notes")).unwrap();
    std::fs::remove_file(job.join("inbox")).unwrap();
    sweep_retired_dir(&live, t, Path::new("job"));
    assert!(!there(&job));
    assert!(there(&t.join("mine/inbox/keep")));

    let job = t.join("job2");
    std::fs::create_dir_all(t.join("elsewhere/runs")).unwrap();
    write(&t.join("elsewhere/daemon.log"));
    write(&t.join("mine/bin/aterm"));
    write(&t.join("marker"));
    write(&job.join("cache"));
    symlink(t.join("elsewhere"), job.join("state")).unwrap();
    symlink(t.join("mine/bin"), job.join("bin")).unwrap();
    symlink(t.join("marker"), job.join(".metadata_never_index")).unwrap();
    sweep_retired_dir(&live, t, Path::new("job2"));
    for kept in [
        "job2/state",
        "job2/bin",
        "job2/cache",
        "job2/.metadata_never_index",
        "elsewhere/runs",
        "elsewhere/daemon.log",
        "mine/bin/aterm",
        "marker",
    ] {
        assert!(there(&t.join(kept)), "{kept}");
    }

    for (i, prefix, tail) in [
        (0, ".local", "share/aterm-machines"),
        (1, ".local/share", "aterm-machines"),
        (2, RETIRED_AGENT_DIR, ""),
    ] {
        let home = t.join(format!("home-{i}"));
        let outside = t.join(format!("outside-{i}"));
        let alias = home.join(prefix);
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        let marker = outside.join(tail).join("bin/aterm");
        write(&marker);
        symlink(&outside, &alias).unwrap();
        sweep_retired_dir(&live, &home, Path::new(RETIRED_AGENT_DIR));
        assert!(there(&alias), "the ancestor alias stays");
        assert_eq!(std::fs::read(&marker).unwrap(), b"x", "external data stays");
    }
    for (i, prefix, tail) in [
        (0, "Library", "LaunchAgents"),
        (1, "Library/LaunchAgents", ""),
    ] {
        let home = t.join(format!("plist-home-{i}"));
        let outside = t.join(format!("plist-outside-{i}"));
        let alias = home.join(prefix);
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        let plist = outside.join(tail).join(format!("{RETIRED_AGENT}.plist"));
        write(&plist);
        symlink(&outside, &alias).unwrap();
        assert!(
            physical_directory_below(&live, &home, Path::new("Library/LaunchAgents")).is_none()
        );
        assert_eq!(std::fs::read(&plist).unwrap(), b"x");
    }
    let home = t.join("physical-home");
    let parent = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&parent).unwrap();
    assert_eq!(
        physical_directory_below(&live, &home, Path::new("Library/LaunchAgents")),
        Some(parent)
    );
    assert!(physical_directory_below(&live, &home, Path::new("missing")).is_none());
    write(&home.join("not-a-directory"));
    assert!(physical_directory_below(&live, &home, Path::new("not-a-directory")).is_none());
}

#[test]
fn the_usage_names_every_verb_main_entry_routes() {
    for verb in VERBS {
        assert!(
            USAGE
                .lines()
                .any(|l| l.split_whitespace().next() == Some(verb)),
            "{verb}"
        );
    }
    assert_eq!(
        main_entry(vec![OsString::from("help"), OsString::from("enroll")]),
        ExitCode::SUCCESS
    );
}

#[test]
fn an_unknown_flag_or_verb_is_refused_with_status_2() {
    for args in [
        &["--aterm-front-door-routing-probe"][..],
        &["frobnicate"],
        &["scan", "--bogus"],
    ] {
        let code = main_entry(args.iter().map(OsString::from).collect());
        assert_eq!(code, ExitCode::from(2), "{args:?}");
    }
    assert_eq!(
        main_entry(vec![OsString::from("--help")]),
        ExitCode::SUCCESS
    );
}

#[test]
fn only_takes_known_table_names_and_refuses_the_rest_with_the_list() {
    let o = parse(
        "apply",
        &[
            "--all".into(),
            "--only".into(),
            "names, power".into(),
            "--only".into(),
            "names".into(),
        ],
    )
    .unwrap();
    assert_eq!(o.only, ["names", "power"]);
    let err = parse("plan", &["--only".into(), "nmaes".into()]).unwrap_err();
    assert!(
        err.contains("\"nmaes\"") && err.contains("names, sharing"),
        "{err}"
    );
    assert!(parse("status", &["--only".into(), "names".into()]).is_err());
}

#[test]
fn outstanding_refuses_json_rather_than_print_text_a_script_cannot_parse() {
    let sys = Fake::root();
    sys.file("/ctl/machines/machines.toml", b"schema = 1\n");
    let paths = || Paths {
        root: std::path::PathBuf::from("/ctl/machines"),
    };
    assert_eq!(
        controller(&sys, paths(), "outstanding", &["--json".to_owned()]),
        ExitCode::from(2)
    );
    assert_eq!(
        controller(&sys, paths(), "outstanding", &[]),
        ExitCode::SUCCESS
    );
}

/// `here apply --plain` reaches the plain run wherever the flag is spelled (a run without
/// root ends on the other class's code, not the usage refusal); with a baseline file, with
/// `plan`, or with `--json` it is refused, so the root half never reads a file at all.
#[test]
fn here_takes_plain_anywhere_and_never_with_a_baseline_file() {
    let args = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let mut user = Fake::default();
    user.euid = 501;
    let other = ExitCode::from(crate::init_clt::Unfinished::Other.code().unwrap());
    for words in [
        &["apply", "--plain"][..],
        &["--plain", "apply"],
        &["apply", "--plain", "--root-steps-only"],
        &["--root-steps-only", "apply", "--plain"],
        &["apply", "--plain", "--name-rule-only"],
    ] {
        assert_eq!(here(&user, &args(words)), other, "{words:?}");
    }
    for words in [
        &["apply", "--plain", "/b/clt.toml"][..],
        &["apply", "/b/clt.toml", "--plain"],
        &["plan", "--plain"],
        &["apply", "--json", "--plain"],
        &["apply", "--root-steps-only"],
        &["apply", "--root-steps-only", "/b/clt.toml"],
        &["apply", "--plain", "--root-steps-only", "/b/clt.toml"],
        &["apply", "--name-rule-only"],
        &["plan", "--plain", "--name-rule-only"],
        &["apply", "--plain", "--name-rule-only", "/b/clt.toml"],
        &["apply", "--plain", "--name-rule-only", "--root-steps-only"],
        &["apply", "--plain", "--name-rule-only", "--name", "m85"],
        &["apply", "--plain", "--name-rule-only", "--nickname", "Dog"],
    ] {
        assert_eq!(here(&user, &args(words)), ExitCode::from(2), "{words:?}");
    }
    assert_eq!(
        here(&user, &args(&["apply", "/b/clt.toml"])),
        ExitCode::from(2)
    );
    assert!(user.calls().is_empty(), "{:?}", user.calls());
}

/// `--name` takes a bare swarm label beside `--plain` alone (it reaches the plain run, which
/// without root ends on the other class); free text, a second `--name`, or `--name` on a
/// baseline file's run is the usage refusal, before anything runs.
#[test]
fn here_takes_name_with_plain_alone_and_only_a_bare_swarm_label() {
    let args = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let mut user = Fake::default();
    user.euid = 501;
    let other = ExitCode::from(crate::init_clt::Unfinished::Other.code().unwrap());
    for words in [
        &["apply", "--plain", "--name", "m42"][..],
        &["--name", "m7", "apply", "--plain"],
        &["apply", "--plain", "--leave-developer-mode"],
        &[
            "apply",
            "--plain",
            "--name",
            "m42",
            "--leave-developer-mode",
        ],
        &["apply", "--plain", "--name", "m85", "--nickname", "Dog"],
        &[
            "--nickname",
            "Blue Heron",
            "--name",
            "m85",
            "apply",
            "--plain",
        ],
    ] {
        assert_eq!(here(&user, &args(words)), other, "{words:?}");
    }
    for words in [
        &["apply", "--plain", "--name"][..],
        &["apply", "--plain", "--name", "M42"],
        &["apply", "--plain", "--name", "m042"],
        &["apply", "--plain", "--name", "m42-studio"],
        &["apply", "--plain", "--name", "Mac-mini-M4"],
        &["apply", "--plain", "--name", "m4", "--name", "m5"],
        &["apply", "/b/clt.toml", "--name", "m42"],
        &["apply", "/b/clt.toml", "--leave-developer-mode"],
        &["apply", "--plain", "--nickname", "Dog"],
        &["apply", "--plain", "--name", "m85", "--nickname"],
        &["apply", "--plain", "--name", "m85", "--nickname", "--bad"],
        &[
            "apply",
            "--plain",
            "--name",
            "m85",
            "--nickname",
            "Dog",
            "--nickname",
            "Cat",
        ],
    ] {
        assert_eq!(here(&user, &args(words)), ExitCode::from(2), "{words:?}");
    }
    assert!(user.calls().is_empty(), "{:?}", user.calls());
}

#[test]
fn hostile_root_naming_inputs_are_refused_before_any_operation() {
    let user = Fake::root();
    for (flag, value) in [
        ("--name", "m00".to_owned()),
        ("--name", "m0 ;".to_owned()),
        ("--name", "ｍ0".to_owned()),
        ("--name", "m０".to_owned()),
        ("--name", "m0\0".to_owned()),
        ("--nickname", "研".repeat(22)),
        ("--nickname", "Dog\0".to_owned()),
        ("--nickname", "Dog\u{202e}".to_owned()),
    ] {
        let mut args: Vec<String> = ["apply", "--plain"].map(str::to_owned).into();
        if flag == "--nickname" {
            args.extend(["--name".to_owned(), "m0".to_owned()]);
        }
        args.extend([flag.to_owned(), value.clone()]);
        assert_eq!(here(&user, &args), ExitCode::from(2), "{flag} {value:?}");
        assert!(user.calls().is_empty(), "{:?}", user.calls());
    }
}
