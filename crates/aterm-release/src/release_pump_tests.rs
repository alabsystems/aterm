// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn host(repo: &Path, declared: &[&str], config: &str) -> Host {
    Host {
        exe: PathBuf::from("/usr/bin/false"),
        repo: repo.to_path_buf(),
        release: repo.join(".git/aterm-release"),
        declared: declared.iter().map(ToString::to_string).collect(),
        handed: Handed::default(),
        adopt: false,
        config: config.to_string(),
        candidate: None,
        tip: String::new(),
    }
}

/// THE LINES THE PUMP STARTS TAKE A LINUX DECLARATION AND NOTHING ELSE FROM THE CONFIG. Its own
/// flags win; else main's `PUMP_LINUX_DEFAULT`, held to a release's Linux flags, so a config
/// line can never hand the pump `--bump`, `--forget` or a commit.
#[test]
fn the_config_supplies_only_a_linux_declaration() {
    let nowhere = Path::new("/nonexistent");
    let line = |value: &str| format!("{}=\"{value}\"\n", pump::LINUX_KEY);
    assert_eq!(
        host(nowhere, &["--mac-only"], &line("--linux-artifacts /d")).linux(),
        Ok(vec!["--mac-only".to_string()]),
        "the pump's own declaration wins"
    );
    assert_eq!(
        host(nowhere, &[], &line("--mac-only")).linux(),
        Ok(vec!["--mac-only".to_string()])
    );
    assert_eq!(
        host(
            nowhere,
            &[],
            &line("--linux-artifacts /d --linux-target aarch64")
        )
        .linux()
        .map(|words| words.join(" ")),
        Ok("--linux-artifacts /d --linux-target aarch64".to_string())
    );
    for refused in [
        "--mac-only --bump",
        "--mac-only --forget",
        "--mac-only --commit abc --supersede",
        "--mac-only --dry-run",
        "--linux-target aarch64",
        "--verbose",
        "",
    ] {
        let said = host(nowhere, &[], &line(refused)).linux().unwrap_err();
        assert!(
            said.contains("is not a Linux declaration"),
            "{refused:?}: {said}"
        );
    }
    let unset = host(nowhere, &[], "STAGING=\"x\"\n").linux().unwrap_err();
    assert!(
        unset.contains(pump::LINUX_KEY) && unset.contains("--mac-only"),
        "{unset}"
    );
}

/// THE BASE IS THE COMMIT THAT OPENED THE VERSION, read from main's own history: the newest
/// commit with the bump's subject whose tree declares the version where its parent's does not.
/// A later commit that only mentions the subject, and a version main never opened, name none.
#[cfg(unix)]
#[test]
fn the_commit_that_opened_a_version_is_read_from_mains_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    let git = GitCli::new(&repo);
    let run = |args: &[&str]| git_ok(&git, args).unwrap().stdout_utf8().trim().to_owned();
    let manifest = |version: &str| {
        std::fs::write(
            repo.join("Cargo.toml"),
            format!("[workspace]\n[workspace.package]\nversion = \"{version}\"\n"),
        )
        .unwrap();
    };
    let commit = |subject: &str| {
        run(&["add", "-A"]);
        run(&["commit", "-q", "--allow-empty", "-m", subject]);
        run(&["rev-parse", "HEAD"])
    };
    manifest("0.5.0");
    commit("release baseline");
    manifest("0.6.0");
    let opened = commit("release: 0.6.0 — the version and the lockfile move together");
    std::fs::write(repo.join("notes"), "a landing\n").unwrap();
    commit("a landing");
    let echo = commit("release: 0.6.0 — a message that only repeats the subject");
    let tip = commit("another landing");
    assert_ne!(echo, opened);

    let host = host(&repo, &[], "");
    assert_eq!(
        host.opened("0.6.0", &tip),
        Some(opened.clone()),
        "a later commit that repeats the subject opened nothing"
    );
    assert_eq!(host.opened("0.6.0", &opened), Some(opened));
    assert_eq!(host.opened("0.7.0", &tip), None);
    assert_eq!(host.version_at(&tip).as_deref(), Some("0.6.0"));
}

/// `--pump` takes the declaration of the lines it starts and its own modes, never a verb.
#[test]
fn the_pump_takes_no_verb_of_its_own() {
    let parse = |words: &[&str]| {
        let mut args = vec!["release".to_string()];
        args.extend(words.iter().map(ToString::to_string));
        crate::cli::parse(&args)
    };
    let Ok(crate::cli::Cmd::Release(opts)) =
        parse(&["--pump", "--once", "--dry-run", "--mac-only", "--verbose"])
    else {
        panic!("the pump's own flags parse");
    };
    assert!(opts.pump && opts.pump_once && opts.dry_run && opts.linux.mac_only);
    for verb in [
        vec!["--bump"],
        vec!["--open-next"],
        vec!["--commit", "abc"],
        vec!["--fix", "abc"],
        vec!["--prepare"],
        vec!["--qualify"],
        vec!["--forget"],
        vec!["--abandon", "v0.6.0"],
        vec!["--prefreeze"],
    ] {
        let mut words = vec!["--pump"];
        words.extend(verb.iter().copied());
        assert!(parse(&words).is_err(), "{verb:?}");
    }
    assert!(parse(&["--once"]).is_err(), "--once is the pump's");
    assert!(parse(&["--adopt"]).is_err(), "--adopt is the pump's");
    let Ok(crate::cli::Cmd::Release(opts)) = parse(&["--pump", "--adopt", "--mac-only"]) else {
        panic!("--pump --adopt parses");
    };
    assert!(opts.pump_adopt);
    let Ok(crate::cli::Cmd::Release(opts)) = parse(&["--pump", "--mac-only"]) else {
        panic!("--pump parses");
    };
    assert!(
        !opts.pump_adopt,
        "taking over another holder's line is opt-in"
    );
}

/// EVERY ARGV THE PUMP RUNS PARSES. Each verb, with every flag the pump can be started with,
/// is a command line the release's own parser accepts as that verb: a usage error (exit 2) is
/// a refusal the pump retries for good, which is how `--open-next --release-credentials`
/// would have stopped the loop after one release.
/// NEGATIVE CONTROL: handing `--release-credentials` to `--open-next` fails its parse here.
#[test]
fn every_argv_the_pump_runs_parses_as_its_verb() {
    let handed = Handed {
        engine: Some("/engine".into()),
        credentials: Some("/profile.toml".into()),
        verbose: true,
    };
    let linux = vec!["--mac-only".to_string()];
    let commit = "c".repeat(40);
    for verb in [
        Verb::OpenNext,
        Verb::Start(commit.clone()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix(commit.clone()),
    ] {
        let argv = verb.argv(&linux, &handed);
        let opts = match crate::cli::parse(&argv) {
            Ok(crate::cli::Cmd::Release(opts)) => opts,
            Ok(_) => panic!("{verb:?} is no release: {argv:?}"),
            Err(why) => panic!("{verb:?} does not parse ({why}): {argv:?}"),
        };
        assert!(opts.verbose && opts.engine.is_some(), "{verb:?}");
        assert!(!opts.pump && !opts.bump && !opts.forget && !opts.supersede);
        assert_eq!(opts.open_next, verb == Verb::OpenNext, "{verb:?}");
        assert_eq!(
            opts.prepare,
            !matches!(verb, Verb::OpenNext | Verb::Finish),
            "{verb:?}"
        );
        assert_eq!(
            opts.commit.as_deref(),
            matches!(verb, Verb::Start(_)).then_some(commit.as_str()),
            "{verb:?}"
        );
        assert_eq!(
            opts.fix,
            if matches!(verb, Verb::Fix(_)) {
                vec![commit.clone()]
            } else {
                Vec::new()
            },
            "{verb:?}"
        );
        assert_eq!(
            opts.release_credentials.is_some(),
            verb != Verb::OpenNext,
            "{verb:?}"
        );
    }
    // The control: the flag the pump withholds is the one the parser refuses.
    let mut refused = Verb::OpenNext.argv(&[], &Handed::default());
    refused.extend(["--release-credentials".to_string(), "/p".to_string()]);
    assert!(crate::cli::parse(&refused).is_err());
}

/// THE RUN'S PROGRESS IS ITS LIVE HOLDER'S NEWEST ROW. Of this Mac's release-line records only
/// a live holder of the version the verb runs counts: a dead holder's old row, an unidentified
/// one and another version's are no progress of this run.
#[test]
fn a_runs_progress_is_the_newest_row_of_a_live_holder_of_its_version() {
    let record = |alive: &str, version: &str, at: u64, p95: u64| {
        let data = format!(
            r#"{{"line":{{"version":"{version}","base":"b","hops":[]}},"holder":{{"pid":7}},"step":"qualify","boundary":"test aterm-gui","at":{at},"p95_s":{p95},"rows":[]}}"#
        );
        format!(
            "release_line alive={alive} data={}",
            aterm_codec::pct::encode(&data)
        )
    };
    let lines = vec![
        record("0", "0.6.0", 900, 0),
        record("1", "0.6.0", 500, 240),
        record("1", "0.6.0", 700, 120),
        record("-", "0.6.0", 950, 0),
        record("1", "0.7.0", 990, 0),
        "release_line alive=1 data=not-json".to_string(),
        "another survey fact".to_string(),
    ];
    assert_eq!(
        newest_row(&lines, "0.6.0"),
        Some(Progress {
            at: 700,
            p95_s: 120,
            boundary: "qualify — test aterm-gui".into(),
        })
    );
    assert_eq!(newest_row(&lines, "0.5.0"), None);
    assert_eq!(newest_row(&lines[..1], "0.6.0"), None, "a dead holder");
}

/// THE INSTALLED PUMP IS AN INTERACTIVE, KEPT-ALIVE AGENT OF THE RELEASE CLONE. Its executable
/// is the immutable copy, its working directory the clone, and its arguments the pump with the
/// declaration it was installed with: never `--install`, `--once` or `--dry-run`, which would
/// reinstall on every start, or exit after one cycle under KeepAlive.
/// NEGATIVE CONTROL: without ProcessType Interactive the job and every release it starts run
/// at utility QoS, where the paint measurement is starved and each cut ends COULD NOT RUN.
#[test]
fn the_installed_pump_is_an_interactive_kept_alive_agent_of_the_clone() {
    let opts = ReleaseOptions {
        pump: true,
        pump_install: true,
        verbose: true,
        engine: Some("/engine & co".into()),
        linux: crate::release::LinuxDecl {
            mac_only: true,
            ..Default::default()
        },
        ..ReleaseOptions::default()
    };
    let args = service_args(&opts);
    assert_eq!(
        args,
        [
            "release",
            "--pump",
            "--mac-only",
            "--engine",
            "/engine & co",
            "--verbose"
        ]
    );
    let text = manifest(
        Path::new("/state/controllers/1/aterm-release"),
        Path::new("/cut/aterm"),
        Path::new("/cut/aterm/.git/aterm-release"),
        Path::new("/Users//a"),
        "/bin:/custom",
        &args,
    );
    for part in [
        "<key>Label</key><string>systems.alab.aterm-release-pump</string>",
        "<key>ProgramArguments</key><array><string>/state/controllers/1/aterm-release</string>\
         <string>release</string><string>--pump</string><string>--mac-only</string>\
         <string>--engine</string><string>/engine &amp; co</string><string>--verbose</string>\
         </array>",
        "<key>WorkingDirectory</key><string>/cut/aterm</string>",
        "<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>",
        "<key>ProcessType</key><string>Interactive</string>",
        "<key>PATH</key><string>/bin:/custom</string>",
        "<key>StandardOutPath</key><string>/cut/aterm/.git/aterm-release/pump.log</string>",
    ] {
        assert!(text.contains(part), "{part}\n{text}");
    }
    for mode in ["--install", "--once", "--dry-run", "--uninstall"] {
        assert!(!text.contains(mode), "{mode}");
    }

    // The service's own flags: installing takes no one-shot mode, uninstalling nothing else.
    let parse = |words: &[&str]| {
        let mut args = vec!["release".to_string()];
        args.extend(words.iter().map(ToString::to_string));
        crate::cli::parse(&args)
    };
    assert!(parse(&["--pump", "--install", "--mac-only"]).is_ok());
    assert!(parse(&["--pump", "--uninstall"]).is_ok());
    for refused in [
        vec!["--install"],
        vec!["--uninstall"],
        vec!["--pump", "--install", "--once"],
        vec!["--pump", "--install", "--dry-run"],
        vec!["--pump", "--install", "--uninstall"],
        vec!["--pump", "--uninstall", "--mac-only"],
    ] {
        assert!(parse(&refused).is_err(), "{refused:?}");
    }
}

/// A VERB RUNS AS A CHILD IN THE CLONE, MARKED AS THE PUMP'S, AND ITS END IS READ WHOLE: its
/// exit code, and its final line past the detail and event records after it. A stand-in for
/// the cutter prints what it was handed and ends as a COULD NOT RUN does.
#[cfg(unix)]
#[test]
fn a_verb_runs_in_the_clone_as_the_pumps_child_and_its_end_is_read() {
    use pump::World as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("clone");
    std::fs::create_dir_all(&repo).unwrap();
    let cutter = root.path().join("cutter");
    aterm_tempfile::stand_in(
        &cutter,
        "#!/bin/sh\necho \"args: $*\"\necho \"pump=$ATERM_RELEASE_PUMP cwd=$(pwd -P)\"\n\
         echo 'REFUSED at 4/10 qualify: COULD NOT RUN — memory — nothing public. again'\n\
         echo '        | a detail line'\necho 'release-event: {\"event\":\"disk\"}'\nexit 3\n",
    )
    .unwrap();
    let mut host = host(&repo, &["--mac-only"], "");
    host.exe = cutter;
    host.handed = Handed {
        verbose: true,
        ..Handed::default()
    };
    let ran = host.run(&Verb::Start("abc".into()));
    assert_eq!(ran.code, 3);
    assert_eq!(
        ran.last,
        "REFUSED at 4/10 qualify: COULD NOT RUN — memory — nothing public. again"
    );

    // What it was handed, read back from a verb whose last line says it.
    let echo = root.path().join("echo");
    aterm_tempfile::stand_in(
        &echo,
        "#!/bin/sh\necho \"$* pump=$ATERM_RELEASE_PUMP cwd=$(pwd -P)\"\n",
    )
    .unwrap();
    host.exe = echo;
    let ran = host.run(&Verb::Start("abc".into()));
    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.last,
        format!(
            "release --prepare --commit abc --mac-only --verbose pump=1 cwd={}",
            repo.canonicalize().unwrap().display()
        )
    );
    // A resume carries no declaration: the journal keeps its own.
    assert!(
        host.run(&Verb::Prepare)
            .last
            .starts_with("release --prepare --verbose pump=1")
    );

    // With no declaration anywhere, a verb that starts a line is not run at all.
    let mut undeclared = self::host(&repo, &[], "");
    undeclared.exe = root.path().join("never-run");
    let ran = undeclared.run(&Verb::Adopt);
    assert_eq!(ran.code, 2);
    assert!(ran.last.contains(pump::LINUX_KEY), "{}", ran.last);
}

/// A TAG ON ORIGIN IS THIS JOURNAL'S CUT ONLY WITH THE CLAIM. The clone that cut a version
/// holds its cut journal (`dist/cut-state.toml`) and owes its after-cut; a clone whose journal
/// of that version claimed nothing lost the line to the clone that did, and the pump must not
/// run the bare release there for good.
/// NEGATIVE CONTROL: with `published` read from the tag alone, the lost journal is an owed
/// after-cut, which the release refuses ("this clone journals no cut of it") on every try.
#[test]
fn a_journal_of_a_version_another_clone_cut_is_lost_not_an_owed_after_cut() {
    let journal = |prepared: bool| Journal {
        version: "0.6.0".into(),
        commit: "c".repeat(40),
        prepared: prepared.then(|| crate::release::Prepared {
            at: 77,
            ..Default::default()
        }),
        ..Journal::default()
    };
    let uncut = journal_seen(journal(true), false, false);
    assert!(uncut.prepared && !uncut.published && !uncut.lost);
    assert_eq!(uncut.prepared_at, 77);
    let mine = journal_seen(journal(true), true, true);
    assert!(mine.published && !mine.lost && !mine.prepared);
    let lost = journal_seen(journal(true), true, false);
    assert!(lost.lost && !lost.published && !lost.prepared);
    assert_eq!((lost.version.as_str(), lost.finished), ("0.6.0", false));

    // The claim is the clone's cut journal naming that version.
    let root = aterm_tempfile::tempdir().unwrap();
    let host = host(root.path(), &[], "");
    assert_eq!(host.claimed("0.6.0"), Ok(false), "no cut journal");
    let dist = root.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(
        dist.join("cut-state.toml"),
        format!(
            "format = 1\nversion = \"0.6.0\"\nbuild_number = 14\ncommit = \"{}\"\ndone = []\n",
            "d".repeat(40)
        ),
    )
    .unwrap();
    assert_eq!(host.claimed("0.6.0"), Ok(true));
    assert_eq!(host.claimed("0.7.0"), Ok(false), "another version's claim");
    std::fs::write(dist.join("cut-state.toml"), "not a journal = [").unwrap();
    assert!(
        host.claimed("0.6.0").is_err(),
        "an unread claim is no answer"
    );
}
