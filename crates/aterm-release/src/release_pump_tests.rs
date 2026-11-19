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
        started: String::new(),
        state: None,
        fact: None,
        unpublished: None,
        claimant: None,
        fleet: None,
        replaced: None,
        unrenewed: None,
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
    assert_eq!(host.opened("0.6.0", &opened), Some(opened.clone()));
    assert_eq!(host.opened("0.7.0", &tip), None);
    assert_eq!(host.version_at(&tip).as_deref(), Some("0.6.0"));
    // The pause before the next version is measured from the opening commit's committer time.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = host.committed_at(&opened).unwrap();
    assert!(at.abs_diff(now) < 24 * 60 * 60, "{at} vs {now}");
    assert_eq!(host.committed_at("no-such-rev"), None);
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
        Verb::Rebase {
            onto: commit.clone(),
            requalify: pump::Requalify::Fix(commit.clone()),
        },
    ] {
        let argv = verb.argv(&linux, &handed);
        let opts = match crate::cli::parse(&argv) {
            Ok(crate::cli::Cmd::Release(opts)) => opts,
            Ok(_) => panic!("{verb:?} is no release: {argv:?}"),
            Err(why) => panic!("{verb:?} does not parse ({why}): {argv:?}"),
        };
        assert!(opts.verbose && opts.engine.is_some(), "{verb:?}");
        assert!(!opts.pump && !opts.bump && !opts.forget);
        assert_eq!(
            opts.supersede,
            matches!(verb, Verb::Rebase { .. }),
            "{verb:?}: only the re-base supersedes"
        );
        assert_eq!(opts.open_next, verb == Verb::OpenNext, "{verb:?}");
        assert_eq!(
            opts.prepare,
            !matches!(verb, Verb::OpenNext | Verb::Finish),
            "{verb:?}"
        );
        assert_eq!(
            opts.commit.as_deref(),
            matches!(verb, Verb::Start(_) | Verb::Rebase { .. }).then_some(commit.as_str()),
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
/// one and another version's are no progress of this run. How many rows the holder completed is
/// read with it (round 4 finding 6: `at` is a renewal its heartbeat also advances), and a record
/// older than the count reads none.
#[test]
fn a_runs_progress_is_the_newest_row_of_a_live_holder_of_its_version() {
    let record = |alive: &str, version: &str, at: u64, p95: u64| {
        let data = format!(
            r#"{{"line":{{"version":"{version}","base":"b","hops":[]}},"holder":{{"pid":7}},"step":"qualify","boundary":"test aterm-gui","at":{at},"p95_s":{p95},"completed":{},"rows":[]}}"#,
            at / 100
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
            rows: 7,
            p95_s: 120,
            boundary: "qualify — test aterm-gui".into(),
        })
    );
    let older = vec![record("1", "0.6.0", 700, 120).replace(r#""completed":7,"#, "")];
    assert_eq!(
        newest_row(&older, "0.6.0").map(|progress| progress.rows),
        Some(0)
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

/// A VERB RUNS AS A CHILD IN THE CLONE, NEVER MARKED AS THE PUMP'S, AND ITS END IS READ WHOLE:
/// its exit code, and its final line past the detail and event records after it. A stand-in for
/// the cutter prints what it was handed (with `ATERM_RELEASE_PUMP`, which no verb the pump runs
/// is given, so a red of it opens no session) and ends as a COULD NOT RUN does.
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
            "release --prepare --commit abc --mac-only --verbose pump= cwd={}",
            repo.canonicalize().unwrap().display()
        )
    );
    // A resumed line whose journal holds no declaration is handed the pump's (0.200.0: a
    // start refused at its first step, and an adoption inside a resume, both leave a journal
    // with none, which `release --prepare` alone refuses "the cut must declare Linux").
    let kept = host.release.join(JOURNAL_FILE);
    let journal = |linux: crate::release::LinuxDecl| {
        Journal {
            version: "0.6.0".into(),
            commit: "c".repeat(40),
            linux,
            ..Journal::default()
        }
        .save(&kept)
        .unwrap();
    };
    journal(crate::release::LinuxDecl::default());
    for (verb, spelled) in [
        (
            Verb::Prepare,
            "release --prepare --mac-only --verbose pump= cwd=",
        ),
        (
            Verb::Fix("abc".into()),
            "release --prepare --fix abc --mac-only --verbose pump= cwd=",
        ),
        (Verb::Finish, "release --mac-only --verbose pump= cwd="),
    ] {
        let ran = host.run(&verb);
        assert!(ran.last.starts_with(spelled), "{verb:?}: {}", ran.last);
    }
    // NEGATIVE CONTROL: a journal that holds a declaration keeps its own, and a flag, which
    // would replace it, is not handed.
    journal(crate::release::LinuxDecl {
        artifacts: Some("/handoff".into()),
        ..Default::default()
    });
    for (verb, spelled) in [
        (Verb::Prepare, "release --prepare --verbose pump= cwd="),
        (Verb::Finish, "release --verbose pump= cwd="),
    ] {
        let ran = host.run(&verb);
        assert!(ran.last.starts_with(spelled), "{verb:?}: {}", ran.last);
    }
    // A start still carries the pump's own, whatever an older journal says.
    assert!(
        host.run(&Verb::Start("abc".into()))
            .last
            .starts_with("release --prepare --commit abc --mac-only --verbose pump= cwd=")
    );
    // With no declaration anywhere, a line that has none is not prepared at all: the run could
    // only be refused, after its first step's checks.
    journal(crate::release::LinuxDecl::default());
    let mut bare = self::host(&repo, &[], "");
    bare.exe = root.path().join("never-run");
    let ran = bare.run(&Verb::Prepare);
    assert_eq!(ran.code, 2);
    assert!(ran.last.contains(pump::LINUX_KEY), "{}", ran.last);
    std::fs::remove_file(&kept).unwrap();

    // With no declaration anywhere, a verb that starts a line is not run at all.
    let mut undeclared = self::host(&repo, &[], "");
    undeclared.exe = root.path().join("never-run");
    let ran = undeclared.run(&Verb::Adopt);
    assert_eq!(ran.code, 2);
    assert!(ran.last.contains(pump::LINUX_KEY), "{}", ran.last);
}

/// A RE-BASE TARGET HOLDS THE LINE'S VERSION, THE FIX AND THE APP'S NEWER COMMITS (UB-15). The
/// newest judged pass past the head takes the line only where it declares the line's version,
/// holds the fix a repair worker named and carries a commit past the head, merges aside, in the
/// app's cone that is not the fix; a candidate.toml's undecided supersession is read from the
/// journal.
/// NEGATIVE CONTROLS: another version, a pass without the fix, one that carries only the fix or
/// only Markdown and change fragments, and history that does not read are each refused, saying
/// why.
#[test]
fn a_rebase_target_holds_the_version_the_fix_and_newer_app_commits() {
    let version = |rev: &str| (rev != "other").then(|| "0.6.0".to_string());
    let holds = |fix: &str, _: &str| fix == "fix";
    let fix_and = |more: &'static str| move |_: &str, _: &str| Some(format!("fix\n{more}"));
    assert_eq!(
        onto_with(
            "0.6.0",
            "head",
            "green",
            Some("fix"),
            &[],
            &version,
            &holds,
            &fix_and("app")
        ),
        Ok(())
    );
    assert_eq!(
        onto_with(
            "0.6.0",
            "head",
            "green",
            None,
            &[],
            &version,
            &holds,
            &|_, _| { Some("app".into()) }
        ),
        Ok(())
    );
    let refused = |result: Result<(), String>, why: &str| {
        let refusal = result.expect_err(why);
        assert!(refusal.contains(why), "{refusal}");
    };
    refused(
        onto_with(
            "0.6.0",
            "head",
            "other",
            None,
            &[],
            &version,
            &holds,
            &fix_and("app"),
        ),
        "declares no workspace version",
    );
    refused(
        onto_with(
            "0.7.0",
            "head",
            "green",
            None,
            &[],
            &version,
            &holds,
            &fix_and("app"),
        ),
        "declares 0.6.0, not the line's 0.7.0",
    );
    refused(
        onto_with(
            "0.6.0",
            "head",
            "green",
            Some("lost"),
            &[],
            &version,
            &holds,
            &fix_and("app"),
        ),
        "does not hold the fix lost",
    );
    refused(
        onto_with(
            "0.6.0",
            "head",
            "green",
            Some("fix"),
            &[],
            &version,
            &holds,
            &|_, _| Some("fix".into()),
        ),
        "carries no commit past head in the app's cone but the fix",
    );
    refused(
        onto_with(
            "0.6.0",
            "head",
            "green",
            None,
            &[],
            &version,
            &holds,
            &|_, _| Some(String::new()),
        ),
        "carries no commit past head in the app's cone, so a hop",
    );
    refused(
        onto_with(
            "0.6.0",
            "head",
            "green",
            None,
            &[],
            &version,
            &holds,
            &|_, _| None,
        ),
        "do not read",
    );
    // A commit a hop of the line already carries is no reason to re-base: its patch is the
    // line's already, though main's original is past the head.
    let hopped = ["early".to_string()];
    refused(
        onto_with(
            "0.6.0",
            "head",
            "green",
            Some("fix"),
            &hopped,
            &version,
            &holds,
            &fix_and("early"),
        ),
        "but the fix and what the line's hops carry",
    );
    assert_eq!(
        onto_with(
            "0.6.0",
            "head",
            "green",
            Some("fix"),
            &hopped,
            &version,
            &holds,
            &|_, _| Some("fix\nearly\napp".into()),
        ),
        Ok(())
    );

    // The undecided supersession reaches the pump's journal.
    let line = |base: &str| crate::line::Line::start("0.6.0", base, 1);
    let journal = Journal {
        version: "0.6.0".into(),
        commit: "g".repeat(40),
        supersede: Some(crate::release::SupersedeTxn {
            head: "h".repeat(40),
            release_line: None,
            previous: line(&"a".repeat(40)),
            replacement: line(&"g".repeat(40)),
            cas_unknown: false,
        }),
        ..Journal::default()
    };
    let seen = journal_seen(journal, (false, false), None, false);
    assert_eq!(
        seen.undecided,
        Some(pump::Undecided {
            from: "a".repeat(40),
            onto: "g".repeat(40)
        })
    );
    assert!(
        !seen.rebased,
        "main's record is main's read, not the journal's"
    );
}

/// THE PUMP NAMES THE HEAD THE FLEET'S CLAIM RECORDS (UB-15). An undecided supersession is
/// claimed at the head it retires until main records the replacement, and a hop carried here
/// whose record has not landed at the newest head main retains; the pump's journal carries that
/// head, which its `pump=` fact names, so a standby matching the fleet's head reads a waiting
/// pump as the line's. A journal whose line extends the claim the same look read (round 4
/// finding 1: a run that recorded its hop on main and stopped before the claim's next step
/// boundary, its journal no longer pending) is claimed at the claim's head. A journal at its
/// claimed head carries none, and neither does one whose read of main fails.
/// NEGATIVE CONTROLS: main's record of the replacement, a journal with no pending line and no
/// claim read, a pending line main retains whole, a claim the journal's line does not extend
/// (another base, another hop, a line ahead of the journal's) and a claim of another version say
/// R.
#[test]
fn the_pump_names_the_head_the_fleets_claim_records() {
    let (a, g, h) = ("a".repeat(40), "g".repeat(40), "h".repeat(40));
    let line = |base: &str| crate::line::Line::start("0.6.0", base, 1);
    let undecided = Journal {
        version: "0.6.0".into(),
        commit: g.clone(),
        supersede: Some(crate::release::SupersedeTxn {
            head: h.clone(),
            release_line: None,
            previous: line(&a),
            replacement: line(&g),
            cas_unknown: false,
        }),
        ..Journal::default()
    };
    let none = |_: &crate::line::Line| -> Option<String> { panic!("no retained read") };
    assert_eq!(
        recorded_head(&undecided, None, &none, None),
        Some(a.clone())
    );
    assert_eq!(
        recorded_head(&undecided, None, &none, Some("other")),
        Some(a.clone())
    );
    // The claim before main's record is the line the supersession retires.
    assert_eq!(
        recorded_head(&undecided, Some(&line(&a)), &none, None),
        Some(a.clone())
    );
    // NEGATIVE CONTROL: main records the replacement, which the claim now stands for.
    assert_eq!(recorded_head(&undecided, None, &none, Some(&g)), None);

    let hop = |commit: &str| crate::line::Hop {
        commit: commit.into(),
        carries: format!("carries-{commit}"),
        why: "a landed fix".into(),
        paths: Vec::new(),
    };
    let hopped = crate::line::Line {
        hops: vec![hop(&h)],
        ..line(&a)
    };
    let mut pending = Journal {
        version: "0.6.0".into(),
        commit: h.clone(),
        line: Some(hopped.clone()),
        ..Journal::default()
    };
    pending.done.push("locally-carried-line".into());
    assert!(pending.pending_line());
    let retained = |_: &crate::line::Line| Some(a.clone());
    assert_eq!(
        recorded_head(&pending, None, &retained, None),
        Some(a.clone())
    );
    assert_eq!(
        journal_seen(pending.clone(), (false, false), Some(a.clone()), false).recorded,
        Some(a.clone())
    );
    // A read of main that fails says R.
    assert_eq!(recorded_head(&pending, None, &|_| None, None), None);
    // NEGATIVE CONTROL: main retains the whole line.
    assert_eq!(
        recorded_head(&pending, None, &|_| Some(h.clone()), None),
        None
    );

    // The hop's record is on main and the journal says nothing is pending, but the run stopped
    // before the claim's next boundary: the claim the same look read is the line before the hop.
    pending.done.clear();
    let recorded = pending.clone();
    assert_eq!(
        recorded_head(&recorded, Some(&line(&a)), &none, None),
        Some(a.clone())
    );
    // The claim that caught up stands for R; so does any claim the journal's line does not extend.
    assert_eq!(recorded_head(&recorded, Some(&hopped), &none, None), None);
    let elsewhere = crate::line::Line {
        hops: vec![hop("i")],
        ..line(&a)
    };
    let ahead = crate::line::Line {
        hops: vec![hop(&h), hop("j")],
        ..line(&a)
    };
    let other_version = crate::line::Line::start("0.7.0", &a, 1);
    for fleet in [&line(&g), &elsewhere, &ahead, &other_version] {
        assert_eq!(
            recorded_head(&recorded, Some(fleet), &none, None),
            None,
            "{fleet:?}"
        );
    }
    // NEGATIVE CONTROL: with no claim read and nothing pending, the journal is claimed at R.
    assert_eq!(recorded_head(&recorded, None, &none, None), None);
}

/// A JOURNAL MAIN'S SUPERSESSION RETIRED IS SEEN RETIRED (UB-15 round 4 finding 3). Where main
/// records a supersession of the journal's version onto another base than its line's (another
/// holder re-based the line), every run of the journal is refused (`line_claim::follow`), and the
/// pump reads it retired from the same scan of main that applies the record to the claim.
/// NEGATIVE CONTROLS: no record, a record of the journal's own base (this clone's own re-base,
/// decided or not), a record of another version and a finished journal are not retired.
#[test]
fn a_journal_mains_supersession_retired_is_seen_retired() {
    let (b, g) = ("b".repeat(40), "g".repeat(40));
    let journal = Journal {
        version: "0.6.0".into(),
        commit: b.clone(),
        line: Some(crate::line::Line::start("0.6.0", &b, 1)),
        ..Journal::default()
    };
    assert!(retired(&journal, Some(&g)));
    assert!(journal_seen(journal.clone(), (false, false), None, true).retired);
    assert!(!retired(&journal, None));
    assert!(!retired(&journal, Some(&b)));
    let mut finished = journal.clone();
    finished
        .done
        .extend(["check".to_string(), "tap-aligned".to_string()]);
    assert!(finished.finished());
    assert!(!retired(&finished, Some(&g)));
    assert!(!journal_seen(finished, (false, false), None, true).retired);
    let root = aterm_tempfile::tempdir().unwrap();
    let mut host = host(root.path(), &[], "");
    assert!(
        !host.seen(journal.clone(), (false, false), None).retired,
        "no look has read main, and this clone's git reads no record"
    );
    host.replaced = Some(("0.7.0".into(), Some(g.clone())));
    assert!(!host.seen(journal.clone(), (false, false), None).retired);
    host.replaced = Some(("0.6.0".into(), None));
    assert!(!host.seen(journal.clone(), (false, false), None).retired);
    host.replaced = Some(("0.6.0".into(), Some(g)));
    assert!(host.seen(journal, (false, false), None).retired);
}

/// A LOOK THAT CANNOT READ MAIN'S CONFIG DECIDES NOTHING (UB-15). main's `publish/config.sh` is
/// what main asks of the pump (whether it publishes, its pause between versions, a run's handed
/// declarations): a tip that holds none, or a read that fails, is an error that fails the look,
/// never an empty config that asks nothing.
/// NEGATIVE CONTROL: a tip that holds the config reads it.
#[cfg(unix)]
#[test]
fn a_config_main_does_not_hold_fails_the_look() {
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
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "no config"]);
    let bare = run(&["rev-parse", "HEAD"]);
    let host = host(&repo, &[], "");
    let refusal = host.config_at(&bare).unwrap_err();
    assert!(
        refusal.contains("publish/config.sh does not read"),
        "{refusal}"
    );
    assert!(host.config_at("not-a-commit").is_err());
    std::fs::create_dir_all(repo.join("publish")).unwrap();
    std::fs::write(
        repo.join("publish/config.sh"),
        "PUMP_LINUX_DEFAULT=\"--mac-only\"\n",
    )
    .unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "config"]);
    let tip = run(&["rev-parse", "HEAD"]);
    assert_eq!(
        host.config_at(&tip),
        Ok("PUMP_LINUX_DEFAULT=\"--mac-only\"".to_string())
    );
}

/// BEFORE A LOOK HAS READ MAIN, A JOURNAL IS JUDGED BY MAIN'S RECORD AS THIS CLONE'S GIT READS IT
/// (UB-15). The pump's first fact, and a cycle whose read of main failed, name the head the claim
/// records for an undecided supersession (the head it retires while main records nothing, R once
/// main records the replacement) and say a journal main's record retired is retired. A look that
/// read main's claim answers for its own version instead.
/// NEGATIVE CONTROL: a look that found no record of the version keeps the retired head and
/// retires nothing, whatever this clone's git holds.
#[cfg(unix)]
#[test]
fn before_a_look_an_undecided_supersession_is_judged_by_mains_record() {
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
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "base"]);
    run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let a = run(&["rev-parse", "HEAD"]);
    let g = "g".repeat(40);
    let line = |base: &str| crate::line::Line::start("0.6.0", base, 1);
    let undecided = Journal {
        version: "0.6.0".into(),
        commit: g.clone(),
        supersede: Some(crate::release::SupersedeTxn {
            head: g.clone(),
            release_line: None,
            previous: line(&a),
            replacement: line(&g),
            cas_unknown: false,
        }),
        ..Journal::default()
    };
    // Another clone's journal of the line before the re-base, which main's record retires.
    let before = Journal {
        version: "0.6.0".into(),
        commit: a.clone(),
        line: Some(line(&a)),
        ..Journal::default()
    };
    let mut host = host(&repo, &[], "");
    let recorded = |host: &Host| host.seen(undecided.clone(), (false, false), None).recorded;
    let retired = |host: &Host| host.seen(before.clone(), (false, false), None).retired;
    assert_eq!(recorded(&host), Some(a.clone()));
    assert!(!retired(&host));
    let record = crate::line::Supersession {
        previous: line(&a),
        replacement: line(&g),
        reason: "a re-base".into(),
    };
    let message = format!(
        "release line v0.6.0: supersede\n\nAterm-Line-Supersession: {}\n",
        aterm_json::to_string(&record).unwrap()
    );
    run(&["commit", "-q", "--allow-empty", "-m", &message]);
    run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert_eq!(recorded(&host), None);
    assert!(retired(&host));
    // A look of another version leaves this one to git.
    host.replaced = Some(("0.7.0".into(), None));
    assert_eq!(recorded(&host), None);
    assert!(retired(&host));
    // NEGATIVE CONTROL: the look read main's record of this version, and found none.
    host.replaced = Some(("0.6.0".into(), None));
    assert_eq!(recorded(&host), Some(a));
    assert!(!retired(&host));
}

/// THE APP'S CONE IS READ FROM GIT, WITH WHAT THE HOPS CARRY LEFT OUT (UB-15). The re-base
/// target's newer commits are main's non-merge commits past the head, read by the very command
/// `World::onto` runs: Markdown anywhere (the root's included) and `changes/` fragments are no
/// app change, a root file merely named `changes…` is one, a change landed through a merge's
/// side branch is found, and a hopped fix's original, which main's history holds past a hop
/// head, adds nothing the line lacks.
/// NEGATIVE CONTROLS: the same hopped line read with no hops left out takes the pass that adds
/// nothing; one app commit landed after it makes the pass a target.
#[cfg(unix)]
#[test]
fn the_app_cone_is_read_from_git_with_the_hops_left_out() {
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
    let touch = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let commit = |subject: &str| {
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", subject]);
        run(&["rev-parse", "HEAD"])
    };
    touch(
        "Cargo.toml",
        "[workspace]\n[workspace.package]\nversion = \"0.6.0\"\n",
    );
    touch("crates/x/src/lib.rs", "pub fn x() {}\n");
    let head = commit("release: 0.6.0");
    let target = |green: &str, fix: Option<&str>, hops: &[String]| {
        onto_in(&repo, "0.6.0", &head, green, fix, hops)
    };
    touch("README.md", "words\n");
    commit("the root's Markdown");
    touch("docs/a/b.md", "words\n");
    commit("a design's Markdown");
    let words = {
        touch("changes/fixed/x.md", "- words\n");
        commit("a change fragment")
    };
    let refusal = target(&words, None, &[]).unwrap_err();
    assert!(refusal.contains("carries no commit past"), "{refusal}");
    touch("changes.txt", "not a fragment\n");
    let named = commit("a root file named like the fragments' folder");
    assert_eq!(target(&named, None, &[]), Ok(()));
    // A change landed through a merge's side branch.
    run(&["reset", "-q", "--hard", &words]);
    run(&["checkout", "-q", "-b", "side"]);
    touch("crates/x/src/lib.rs", "pub fn x() -> u8 { 1 }\n");
    commit("an app change on a side branch");
    run(&["checkout", "-q", "-"]);
    run(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "land the side branch",
        "side",
    ]);
    let landed = run(&["rev-parse", "HEAD"]);
    assert_eq!(target(&landed, None, &[]), Ok(()));

    // A hopped line: main lands F1, the line carries it as hop H1 onto its head, main records
    // the line (a merge of H1), then lands the fix F3.
    run(&["reset", "-q", "--hard", &head]);
    touch("crates/x/src/a.rs", "pub fn a() {}\n");
    let f1 = commit("F1: an app fix main lands");
    run(&["checkout", "-q", "-b", "line", &head]);
    run(&["cherry-pick", "--no-commit", &f1]);
    let h1 = commit(&format!("release line v0.6.0 hop 1: carries {f1} (F1)"));
    run(&["checkout", "-q", "-"]);
    run(&["merge", "-q", "--no-ff", "-m", "record the line", "line"]);
    touch("crates/x/src/b.rs", "pub fn b() {}\n");
    let f3 = commit("F3: the fix a worker names");
    let line =
        |green: &str, hops: &[String]| onto_in(&repo, "0.6.0", &h1, green, Some(f3.as_str()), hops);
    let hops = [f1.clone()];
    let refusal = line(&f3, &hops).unwrap_err();
    assert!(
        refusal.contains("but the fix and what the line's hops carry"),
        "{refusal}"
    );
    // NEGATIVE CONTROL: with no hops left out, F1's original reads as a newer app commit.
    assert_eq!(line(&f3, &[]), Ok(()));
    touch("crates/x/src/c.rs", "pub fn c() {}\n");
    let f4 = commit("F4: an app change past the fix");
    assert_eq!(line(&f4, &hops), Ok(()));
}

/// A CHILD THE PUMP CANNOT START IS A COULD NOT RUN (UB-15): exit 3, which decided nothing, so a
/// re-base whose child found no process slot is run again by that exit's backoff rather than
/// spent.
/// NEGATIVE CONTROL: a run the pump has no Linux declaration for is a refusal (exit 2), which it
/// can only be after its first step's checks, never a could not run.
#[cfg(unix)]
#[test]
fn a_verb_whose_child_cannot_start_could_not_run() {
    use pump::World as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("clone");
    std::fs::create_dir_all(&repo).unwrap();
    let mut host = host(&repo, &["--mac-only"], "");
    host.exe = root.path().join("never-run");
    let ran = host.run(&Verb::Rebase {
        onto: "g".repeat(40),
        requalify: pump::Requalify::Fix("f".repeat(40)),
    });
    assert_eq!(ran.code, 3, "{}", ran.last);
    assert!(ran.last.contains("cannot start"), "{}", ran.last);
    assert!(!ran.started, "no release process ran");
    let mut undeclared = self::host(&repo, &[], "");
    undeclared.exe = root.path().join("never-run");
    let unstarted = undeclared.run(&Verb::Adopt);
    assert_eq!(unstarted.code, 2);
    assert!(!unstarted.started, "no release process ran");
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
    let uncut = journal_seen(journal(true), (false, false), None, false);
    assert!(uncut.prepared && !uncut.published && !uncut.lost);
    assert_eq!(uncut.prepared_at, 77);
    let mine = journal_seen(journal(true), (true, true), None, false);
    assert!(mine.published && !mine.lost && !mine.prepared);
    let lost = journal_seen(journal(true), (true, false), None, false);
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

/// AN INHERITED PUMP MARK REACHES NO VERB (the judge's correction 2). The cutter opens a
/// session for a red wherever `ATERM_RELEASE_PUMP` is in its environment, with any value
/// (`release_steps.rs`, `repair::spawner`). Every verb's child is built with the variable
/// removed, so neither the pump nor the shell, agent or agent plist that started it can turn
/// sessions back on; the stand-in run above shows the child is given no value.
/// NEGATIVE CONTROL: a command that only leaves the variable unset lists no edit of it, and its
/// child would inherit the pump's value.
#[test]
fn every_verb_runs_with_the_pump_mark_removed() {
    let host = host(Path::new("/nonexistent"), &["--mac-only"], "");
    let mark = std::ffi::OsStr::new(repair::PUMP_ENV);
    for verb in [
        Verb::OpenNext,
        Verb::Start("abc".into()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix("abc".into()),
    ] {
        let command = host.command(&verb, &["--mac-only".to_string()]);
        let edits: Vec<_> = command.get_envs().collect();
        assert!(edits.contains(&(mark, None)), "{verb:?}: {edits:?}");
        assert_eq!(command.get_current_dir(), Some(Path::new("/nonexistent")));
    }
    let unset = aterm_uds::spawnfd::command("/usr/bin/true");
    assert!(unset.get_envs().all(|(key, _)| key != mark));
}

/// ONLY A RE-BASE'S CHILD IS MARKED TO ADMIT THE LINE'S HOLDER (UB-15 finding 7). The pump judges
/// the line free when it looks, minutes before its re-base's child takes the claim; the child of
/// `Verb::Rebase` alone carries `pump::REBASE_ENV=1`, under which the release's `--supersede`
/// takes the claim only from a holder a contender could take it from
/// (`line_claim::Take::Rebase`), and every other verb's child has it removed, so a session's
/// `--supersede` started from a shell that inherited it is still a session's.
/// NEGATIVE CONTROL: a command that only leaves the variable unset lists no edit of it.
#[test]
fn only_a_rebases_child_is_marked_to_admit_the_lines_holder() {
    let host = host(Path::new("/nonexistent"), &["--mac-only"], "");
    let mark = std::ffi::OsStr::new(pump::REBASE_ENV);
    let rebase = host.command(
        &Verb::Rebase {
            onto: "abc".into(),
            requalify: pump::Requalify::Red("ab12".into()),
        },
        &[],
    );
    let edits: Vec<_> = rebase.get_envs().collect();
    assert!(
        edits.contains(&(mark, Some(std::ffi::OsStr::new("1")))),
        "{edits:?}"
    );
    for verb in [
        Verb::OpenNext,
        Verb::Start("abc".into()),
        Verb::Adopt,
        Verb::Prepare,
        Verb::Finish,
        Verb::Fix("abc".into()),
    ] {
        let command = host.command(&verb, &["--mac-only".to_string()]);
        let edits: Vec<_> = command.get_envs().collect();
        assert!(edits.contains(&(mark, None)), "{verb:?}: {edits:?}");
    }
    let unset = aterm_uds::spawnfd::command("/usr/bin/true");
    assert!(unset.get_envs().all(|(key, _)| key != mark));
}

/// A RED THE CUTTER HANDS ITS CALLER IS READ WITH THE RUN'S END. The release prints its
/// caller-owned repair line, its briefing, then its final refusal: the pump keeps the line's
/// input hash with the exit and the final line ([`pump::Ran::red`]), and holds the red line.
/// NEGATIVE CONTROL: a refusal that prints no such line names no red.
#[cfg(unix)]
#[test]
fn a_runs_caller_owned_red_is_read_with_its_end() {
    use pump::World as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("clone");
    std::fs::create_dir_all(&repo).unwrap();
    let red = root.path().join("red");
    aterm_tempfile::stand_in(
        &red,
        "#!/bin/sh\necho '[ 1/10] checks          REFUSED  00:25:01  7m34s  (log: /l/01.log)'\n\
         echo 'repair: caller-owned repair (input hash 831fd1c0e6d2); no worker opened'\n\
         echo 'A release cut of aterm stopped at a failing gate or build.'\n\
         echo 'What failed: verify release-input, at step 1/10 checks'\n\
         echo 'REFUSED at 1/10 checks: release input is red — nothing public. land the fix'\n\
         exit 1\n",
    )
    .unwrap();
    let mut host = host(&repo, &["--mac-only"], "");
    host.exe = red;
    let ran = host.run(&Verb::Prepare);
    assert_eq!((ran.code, ran.red.as_deref()), (1, Some("831fd1c0e6d2")));
    assert_eq!(
        ran.last,
        "REFUSED at 1/10 checks: release input is red — nothing public. land the fix"
    );
    let plain = root.path().join("plain");
    aterm_tempfile::stand_in(
        &plain,
        "#!/bin/sh\necho 'REFUSED at 1/10 checks: the clone is not clean — nothing public. x'\n\
         exit 1\n",
    )
    .unwrap();
    host.exe = plain;
    let ran = host.run(&Verb::Prepare);
    assert_eq!((ran.code, ran.red), (1, None));
}

/// THE PUMP'S SURVEY RECORD IS ITS FACT AND ITS OWN PROCESS (UB-15, `pump=`). What a cycle
/// publishes lands in the harness state folder as the record a survey answers, with this
/// process's pid, boot and start for the answering kernel to judge; a record that cannot be
/// written is kept as said until one is written again, and stops nothing. Whether the pump holds
/// the line's journal (`own`, what a standby judges) is in the record. Each clone's pump keeps a
/// record of its own (findings 8 and 14): a `--once` in another clone of the same login writes
/// beside the primary's, and a survey answers both.
/// NEGATIVE CONTROLS: a state folder that is a file writes nothing; the other clone's record does
/// not replace the primary's.
#[test]
fn the_pump_writes_its_fact_as_the_survey_record_of_its_own_process() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut host = host(root.path(), &[], "");
    let fact = pump::Fact {
        holder: "m16 (node 7c1e)".into(),
        line: "v0.6.0@ccccccccc".into(),
        step: "rebase".into(),
        own: true,
        at: 1_234,
        decision: "requalify: the line went red (caller-owned repair, input hash ab12)".into(),
    };
    let blocked = root.path().join("a-file");
    std::fs::write(&blocked, "").unwrap();
    host.state = Some(blocked.clone());
    pump::World::publish(&mut host, &fact);
    pump::World::publish(&mut host, &fact);
    assert!(
        host.unpublished.is_some(),
        "the failure is kept, so it is said once"
    );
    assert!(!blocked.join(aterm_ctl::release_pump::DIRECTORY).exists());
    let state = root.path().join("harness");
    host.state = Some(state.clone());
    pump::World::publish(&mut host, &fact);
    assert_eq!(host.unpublished, None);
    assert_eq!(host.fact.as_ref(), Some(&fact));
    let path = host.record().unwrap();
    assert_eq!(
        path,
        state
            .join(aterm_ctl::release_pump::DIRECTORY)
            .join(record_name(&host.release))
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let record = aterm_ctl::release_pump::Record::from_json(&text).unwrap();
    assert_eq!(
        (
            record.holder.as_str(),
            record.line.as_str(),
            record.step.as_str(),
            record.own,
            record.at
        ),
        ("m16 (node 7c1e)", "v0.6.0@ccccccccc", "rebase", true, 1_234)
    );
    assert_eq!(record.decision, fact.decision);
    assert_eq!(record.pid, std::process::id());
    assert!(record.boot.is_some() && record.start.is_some());

    // Another clone's `--once`, in the same login's harness state, writes a record of its own.
    let other = root.path().join("other-clone");
    let mut once = host_at(&other, &state);
    pump::World::publish(
        &mut once,
        &pump::Fact {
            line: "v0.6.0@ccccccccc".into(),
            step: "wait".into(),
            own: false,
            at: 1_240,
            decision: "one cycle".into(),
            ..pump::Fact::default()
        },
    );
    assert_ne!(once.record(), host.record());
    let rows = aterm_agent::release_progress::pump_rows(&state, 1_300);
    let read: Vec<_> = rows
        .iter()
        .map(|row| aterm_ctl::release_pump::Pump::parse(row).unwrap())
        .collect();
    assert_eq!(
        read.iter()
            .map(|pump| (pump.own, pump.age_s))
            .collect::<Vec<_>>(),
        [(Some(false), 60), (Some(true), 66)],
        "{rows:?}"
    );
}

/// A host of the clone at `repo` that publishes in the harness state folder `state`.
fn host_at(repo: &Path, state: &Path) -> Host {
    let mut host = host(repo, &[], "");
    host.state = Some(state.to_path_buf());
    host
}

/// A PROPOSAL OF A VERSION ANOTHER CLONE CUT IS NO JOURNAL OF A LINE (UB-15). A pump that yielded
/// the line, or a standby whose adopt stopped before its claim, keeps a proposal on which it did
/// nothing; once the other clone cuts that version the pump reads no journal, as the release's
/// own selection drops it, and goes on to open, start or adopt the next version rather than
/// holding a lost journal for good. Where the version is cut the fleet's line is not read.
/// NEGATIVE CONTROLS: a journal with work on it (a pushed bump) stays, lost, for a session's
/// `--forget`; a proposal of a version nobody cut stays unless the fleet's line starts elsewhere.
#[test]
fn a_proposal_of_a_version_another_clone_cut_is_no_journal_of_a_line() {
    let proposal = Journal {
        version: "0.6.0".into(),
        commit: "c".repeat(40),
        started: 100,
        ..Journal::default()
    };
    let unread = || -> Result<bool, String> { panic!("the fleet line is not read") };
    assert_eq!(
        proposal_dropped(&proposal, None, &mut || Ok(true), &mut unread.clone()),
        Ok(true)
    );
    assert_eq!(
        proposal_dropped(&proposal, None, &mut || Ok(false), &mut || Ok(true)),
        Ok(true)
    );
    assert_eq!(
        proposal_dropped(
            &proposal,
            None,
            &mut || Err("down".into()),
            &mut unread.clone()
        ),
        Err("down".into())
    );
    // NEGATIVE CONTROLS: work on it, or a version nobody cut whose fleet line is this one.
    let worked = Journal {
        bumped: true,
        ..proposal.clone()
    };
    assert_eq!(
        proposal_dropped(&worked, None, &mut || Ok(true), &mut || Ok(true)),
        Ok(false)
    );
    assert_eq!(
        proposal_dropped(&proposal, None, &mut || Ok(false), &mut || Ok(false)),
        Ok(false)
    );
}
