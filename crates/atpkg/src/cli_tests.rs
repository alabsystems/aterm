// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// THE PRODUCTION HALF of `cli.rs`, for the source scans below.
///
/// This module lives in `cli_tests.rs`, declared out of line at the end of `cli.rs`
/// (2026-10-06), so the half is `cli.rs` up to that declaration: all of it bar the
/// lines that declare this module, and still found by an anchor that panics.
///
/// The anchor is the test module's OWN attribute. `#[cfg(test)]` — what these
/// scans used to split on — never matched this module, which is
/// `#[cfg(all(test, unix))]`: the split landed on the first scan's own string
/// literal instead, so "production" silently included ~2 800 lines of tests, and
/// the first real `#[cfg(test)]` item to appear in production code (the
/// announcement ledger's reset seam, 2026-09-11) truncated every scan to the
/// first few thousand lines and turned them all vacuous at once. An anchor that
/// can miss is the same defect these scans exist to catch, so this one panics
/// rather than falling back to the whole file. The anchor is the attribute WITH the
/// module line under it, because production code carries the same attribute on its
/// own test seams (the announcement ledger's `reset`, 2026-09-25).
fn production_half(src: &str) -> &str {
    let (before, _) = src
        .split_once("#[cfg(all(test, unix))]\n#[path = \"cli_tests.rs\"]\nmod tests;")
        .expect("cli.rs's test module is gated `#[cfg(all(test, unix))]`");
    assert!(
        before.contains("fn cmd_seed") && !before.contains("mod tests {"),
        "the production half must hold the verbs and none of the tests"
    );
    before
}

/// EVERY FIXTURE PASS RUNS AGAINST AN EMPTY `PATH`.
///
/// One wrapper over [`super::install_default_set_with_path`] for the whole test
/// module (the production wrapper of this name, which read the process `PATH`, was
/// deleted in Phase 5 — every production caller injects its own), rather than
/// sixteen call sites that a seventeenth could then forget: a pass consults
/// `PATH` twice — for a foreign copy ahead of ours (`reconcile_shadowed`) and
/// for a system install that already satisfies a program
/// (`reconcile_system_satisfied_with`) — and a fixture that reads the
/// DEVELOPER's `PATH` is judging a temp prefix against the real machine.
///
/// That is not hypothetical and it is not a flake. On any box with aterm's
/// own toolchain installed — `~/Library/Application Support/aterm/pkg/bin`,
/// i.e. the machine this crate is developed and dogfooded on — `ay` resolves
/// outside the fixture prefix, so the recorded row reads `managed 18 —
/// SHADOWED by …` where the fixture expects `pinned by index 41`, and
/// `a_program_whose_requirement_is_unmet_…` and
/// `an_index_listed_extra_gets_a_consent_stub_…` failed for exactly the
/// people most likely to run them. The shipping behaviour was right the
/// whole time; only the test's environment was ambient.
///
/// An EMPTY value, not the real one filtered: it is the only setting that
/// cannot acquire a foreign binary later. A pass that wants to exercise
/// shadowing calls `super::install_default_set_with_path` and says which
/// `PATH` it means.
///
/// The consent is the first launch's: it shapes only the wire lane's announcement, whose
/// words `net_announcement`'s own tests pin.
fn install_default_set(
    layout: &crate::store::Layout,
    fetcher: &dyn crate::flow::Fetcher,
    anchor: &crate::Anchor,
    cfg: &crate::config::PackagesConfig,
    now: i64,
) -> super::DefaultSetOutcome {
    super::install_default_set_with_path(
        layout,
        fetcher,
        anchor,
        cfg,
        super::Consent::InstallingAterm,
        now,
        None,
        Some(std::ffi::OsStr::new("")),
        &crate::quiet::FlipGate::NOW,
        &std::collections::BTreeSet::new(),
    )
}

/// A FLAG IS NOT A PROGRAM. Measured on a live box: a mistyped
/// `atpkg install --progress-file` recorded the flag as a wanted program,
/// and `doctor` then carried a standing warning about a row that "cannot be
/// a program name". Refusing at the door is the fix; the exit code is the
/// usage code, not the failure code, because the user's command was
/// malformed rather than their system broken.
#[test]
fn install_refuses_a_flag_as_a_program_name() {
    for flag in ["--progress-file", "--wait-lock", "-v", "--default-sett"] {
        let code = super::cmd_install(Some(&flag.to_string()), "install");
        assert_eq!(
            code,
            std::process::ExitCode::from(2),
            "{flag:?} must be refused as a usage error, never recorded"
        );
    }
    // The one flag-shaped argument that IS a verb must reach its branch
    // rather than the refusal. Asserted structurally — in a test process
    // with no root key pinned `--default-set` also exits 2, from the
    // manager gate rather than from this guard, so the exit code cannot
    // tell the two apart.
    let src = include_str!("cli.rs");
    let default_set_at = src
        .find("if program == \"--default-set\" {")
        .expect("the --default-set branch");
    let guard_at = src
        .find("is a command-line flag, not a program name")
        .expect("the flag guard");
    assert!(
        default_set_at < guard_at,
        "--default-set must be answered BEFORE the flag guard, or the \
         documented whole-toolset spelling would refuse itself"
    );
}

/// The `--wait-lock <secs>` edge grammar (2026-09-10), mirror of the
/// `--progress-file` reasoning above: stripped for the three provisioning verbs
/// wherever it sits, the LAST parsable value wins, and an unparsable value — or a
/// bare flag at the END of argv — is consumed and means "no wait", never a program
/// name. (A valueless flag mid-argv eats the next token instead; no spawner
/// produces that shape — see `take_wait_lock_flag`.)
#[test]
fn wait_lock_flag_is_stripped_at_the_edge_and_degrades_to_no_wait() {
    let argv = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    let secs = |n: u64| Some(std::time::Duration::from_secs(n));

    let mut args = argv(&["seed", "--wait-lock", "5"]);
    assert_eq!(super::take_wait_lock_flag(&mut args), secs(5));
    assert_eq!(args, argv(&["seed"]));

    let mut args = argv(&["install", "--default-set", "--wait-lock", "9"]);
    assert_eq!(super::take_wait_lock_flag(&mut args), secs(9));
    assert_eq!(args, argv(&["install", "--default-set"]));

    let mut args = argv(&["update", "--wait-lock"]);
    assert_eq!(super::take_wait_lock_flag(&mut args), None, "bare: no wait");
    assert_eq!(args, argv(&["update"]));

    let mut args = argv(&["seed", "--wait-lock", "soon"]);
    assert_eq!(
        super::take_wait_lock_flag(&mut args),
        None,
        "unparsable: no wait"
    );
    assert_eq!(args, argv(&["seed"]), "…and the value is consumed");

    let mut args = argv(&["seed", "--wait-lock", "1", "--wait-lock", "2"]);
    assert_eq!(super::take_wait_lock_flag(&mut args), secs(2), "LAST wins");
    assert_eq!(args, argv(&["seed"]));

    let mut args = argv(&["update"]);
    assert_eq!(super::take_wait_lock_flag(&mut args), None);
    assert_eq!(args, argv(&["update"]), "absent: untouched");
}

#[test]
fn published_index_hint_is_a_stripped_scheduling_witness_only() {
    let words = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut args = words(&["update", PUBLISHED_INDEX_HINT_FLAG, "45"]);
    assert_eq!(take_published_index_hint_flag(&mut args), Some(45));
    assert_eq!(args, words(&["update"]));
    for value in ["0", "nope"] {
        let mut args = words(&["update", PUBLISHED_INDEX_HINT_FLAG, value]);
        assert_eq!(take_published_index_hint_flag(&mut args), None);
        assert_eq!(args, words(&["update"]));
    }
}

/// THE PACKAGE LOG'S LANES (Phase 4): read off how the pass was spawned — the head
/// watch's flag, the session's detached spawner, the window's queue-and-report flags,
/// the intercept's queue without a progress file — else it was typed. And the exit
/// code an `ExitCode` carries comes back out of it.
#[test]
fn a_pass_names_its_lane_from_how_it_was_spawned() {
    let lane = |hw, wait, progress, detached, verb, operand| {
        super::pass_lane(hw, wait, progress, detached, Some(verb), operand)
    };
    assert_eq!(
        lane(true, true, true, false, "update", Some("claude")),
        "head watch"
    );
    assert_eq!(lane(false, true, true, true, "update", None), "session");
    assert_eq!(lane(false, true, true, false, "update", None), "window");
    assert_eq!(lane(false, true, true, false, "seed", None), "window");
    assert_eq!(
        lane(false, true, false, false, "update", Some("codex")),
        "agent update",
        "the intercept's child: `update <vendor> --wait-lock`, no progress file"
    );
    assert_eq!(
        lane(false, true, false, false, "update", Some("ay")),
        "window"
    );
    assert_eq!(lane(false, false, false, false, "update", None), "typed");
    let intercept = crate::selfupdate::child_args("claude", 1800);
    assert!(
        intercept.iter().any(|a| a == "--wait-lock")
            && !intercept.iter().any(|a| a == "--progress-file"),
        "the rule reads the intercept's argv: {intercept:?}"
    );
    for n in [0u8, 1, 2, 69, 75, 126, 255] {
        assert_eq!(super::exit_code_u8(ExitCode::from(n)), n);
    }
    assert_eq!(
        super::pass_verb_words(&[
            "install".to_string(),
            "--default-set".to_string(),
            "x".to_string()
        ]),
        "install --default-set"
    );
}

/// A PASS-END QUOTES ONLY ITS OWN OUTCOME (review of Phase 4, 2026-09-23). A row written
/// KEEPING the outcome — the head-watch door's `update codex` whose vendor was not
/// reached — moves `updated_at` and carries an earlier pass's sentence; that sentence
/// was logged as this pass's result (`… outcome=up to date (index build 44)`). Now only
/// a pass that wrote an outcome is quoted — the same sentence again included — and a
/// pass that wrote none says none.
#[cfg(unix)]
#[test]
fn a_pass_end_quotes_the_outcome_this_pass_wrote_and_no_other() {
    let layout = temp_layout("pass-end-outcome");
    crate::status::write(
        &layout,
        &crate::Status {
            schema: 1,
            updated_at: "2026-09-20T00:00:00Z".into(),
            outcome: "up to date (index build 44)".into(),
            ..crate::Status::default()
        },
    )
    .unwrap();
    let args = [String::from("update"), String::from("codex")];
    let row = |state: &str| crate::ProgramStatus {
        installed_build: None,
        state: state.into(),
        tree_root: String::new(),
    };
    let pass = PassRecord::begin(layout.clone(), &args);
    record_row_keeping_outcome(
        &layout,
        "codex",
        row("managed 0.156.0 \u{2014} OpenAI's release channel not reached since 2026-09-21"),
    );
    let record = crate::status::read(&layout).unwrap();
    assert_ne!(
        record.updated_at, "2026-09-20T00:00:00Z",
        "the row write moved it"
    );
    assert_eq!(
        record.outcome, "up to date (index build 44)",
        "the sentence was kept"
    );
    assert_eq!(pass.outcome(), "", "…and it is no outcome of this pass");

    let pass = PassRecord::begin(layout.clone(), &args);
    record_status(
        &layout,
        "codex",
        row("error: codex 0.157.0 refused: digest mismatch"),
        "codex 0.157.0 refused: digest mismatch".into(),
    );
    assert_eq!(pass.outcome(), "codex 0.157.0 refused: digest mismatch");

    // The same sentence written again is this pass's too (the record stamped earlier,
    // so the write is seen to land).
    let mut earlier = crate::status::read(&layout).unwrap();
    earlier.updated_at = "2026-09-21T00:00:00Z".into();
    crate::status::write(&layout, &earlier).unwrap();
    let pass = PassRecord::begin(layout.clone(), &args);
    record_outcome(&layout, "codex 0.157.0 refused: digest mismatch".into());
    assert_eq!(pass.outcome(), "codex 0.157.0 refused: digest mismatch");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `--defer-busy-flip` (gap #12(b)) is the unattended lanes' opt-in to hold the
/// toolchain's flip for quiet: stripped at the edge wherever it sits, never an operand,
/// and without it — a person's `aterm pkg update` — the policy is to flip at once.
#[test]
fn the_defer_busy_flip_flag_is_stripped_and_picks_the_policy() {
    let argv = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut args = argv(&["update", "--wait-lock", "1800", "--defer-busy-flip"]);
    assert!(super::take_flag(&mut args, super::DEFER_BUSY_FLIP_FLAG));
    assert_eq!(args, argv(&["update", "--wait-lock", "1800"]));
    let mut args = argv(&["update"]);
    assert!(!super::take_flag(&mut args, super::DEFER_BUSY_FLIP_FLAG));
    assert_eq!(args, argv(&["update"]), "absent: untouched");
    assert_eq!(super::DEFER_BUSY_FLIP_FLAG, "--defer-busy-flip");
    // This test process never saw the flag: a person's pass.
    assert_eq!(super::flip_policy(), crate::quiet::FlipPolicy::Now);
}

/// THE HELD FLIP'S ROWS (gap #12(b)): a toolchain flip held for quiet is a state, never
/// a failure — each member it would move gets a `deferred:` row naming the staged
/// build, the live one and the latest it waits until, doctor counts none of it a
/// problem, and the UpToDate arm lifts the row once the wait is over.
#[test]
fn a_held_toolchain_flip_writes_deferred_rows_and_counts_no_failure() {
    let layout = temp_layout("flip-deferred-rows");
    let t0 = 1_790_000_000;
    assert!(crate::quiet::note_deferred(
        &layout,
        "rustc",
        t0,
        t0,
        &crate::quiet::Busy::Running {
            exe: std::path::PathBuf::from("/x/targo"),
        },
        &crate::quiet::Moves {
            from: std::collections::BTreeMap::from([("trust".to_string(), 4800u64)]),
            to: std::collections::BTreeMap::from([("trust".to_string(), 4821u64)]),
            assets: std::collections::BTreeMap::new(),
        },
    ));
    let installed = std::collections::BTreeMap::from([("trust".to_string(), 4800u64)]);
    let report = crate::ChannelApplyReport {
        index_build: 41,
        roster_seq: 1,
        groups: vec![(
            crate::Group {
                group: Some("rustc".into()),
                members: vec!["trust".into()],
            },
            crate::TxnOutcome::Deferred {
                members: vec!["trust".into()],
                why: "/x/targo is running from it".into(),
                since: t0,
            },
        )],
        applied: std::collections::BTreeMap::new(),
        skipped_linked: Vec::new(),
        resolved_assets: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        report_channel_apply(&layout, &installed, &report, &[], true),
        0
    );
    let status = crate::status::read(&layout).expect("recorded");
    let row = &status.programs["trust"];
    assert_eq!(
        row.installed_build,
        Some(4800),
        "the live build stays on the row"
    );
    assert_eq!(
        row.state,
        format!(
            "deferred: build 4821 is staged and installs when nothing is using the \
             toolchain, by {} at the latest while an aterm window or terminal session is \
             open; staying on build 4800",
            crate::quiet::clock(t0 + crate::quiet::CEILING_SECS)
        )
    );
    assert_eq!(
        crate::state::not_current(&row.state).map(|(word, _)| word),
        Some("deferred")
    );
    assert!(
        crate::doctor::recorded_problems(Some(&status)).is_empty(),
        "a held flip is never a doctor fault"
    );
    assert!(
        up_to_date_row_needs_rewrite(Some(&row.state), 4800, 41),
        "an UpToDate pass lifts it"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A GROUP PINNED MID-WAIT ENDS EVERY MEMBER'S `deferred:` ROW (2026-09-26): the pin
/// ends the held flip, and a sibling the flip would have moved — not pinned itself —
/// stops saying its staged build installs when the toolchain is quiet: it is held by the
/// pin, and says whose. A sibling whose row said something else keeps it.
#[test]
fn a_group_pinned_mid_wait_rewrites_its_siblings_deferred_rows() {
    let layout = temp_layout("flip-deferred-pinned");
    let t0 = 1_790_000_000;
    let group = crate::Group {
        group: Some("rustc".into()),
        members: vec!["trust".into(), "trust-ir".into(), "trust-vc".into()],
    };
    let installed = std::collections::BTreeMap::from([
        ("trust".to_string(), 4800u64),
        ("trust-ir".to_string(), 40),
        ("trust-vc".to_string(), 7),
    ]);
    let report = |outcome| crate::ChannelApplyReport {
        index_build: 41,
        roster_seq: 1,
        groups: vec![(group.clone(), outcome)],
        applied: std::collections::BTreeMap::new(),
        skipped_linked: Vec::new(),
        resolved_assets: std::collections::BTreeMap::new(),
    };
    // The wait: trust and trust-ir move; trust-vc is already at its pin.
    assert!(crate::quiet::note_deferred(
        &layout,
        "rustc",
        t0,
        t0,
        &crate::quiet::Busy::Quiet,
        &crate::quiet::Moves {
            from: std::collections::BTreeMap::from([
                ("trust".to_string(), 4800u64),
                ("trust-ir".to_string(), 40),
            ]),
            to: std::collections::BTreeMap::from([
                ("trust".to_string(), 4821u64),
                ("trust-ir".to_string(), 41),
            ]),
            assets: std::collections::BTreeMap::new(),
        },
    ));
    record_status(
        &layout,
        "trust-vc",
        crate::ProgramStatus {
            installed_build: Some(7),
            state: crate::state::managed(7, 41),
            tree_root: String::new(),
        },
        "group rustc: up to date".to_string(),
    );
    let deferred = crate::TxnOutcome::Deferred {
        members: vec!["trust".into(), "trust-ir".into()],
        why: "/x/targo is running from it".into(),
        since: t0,
    };
    report_channel_apply(&layout, &installed, &report(deferred), &[], true);
    let rows = |p: &str| {
        crate::status::read(&layout).unwrap().programs[p]
            .state
            .clone()
    };
    assert!(rows("trust").starts_with(crate::state::FLIP_DEFERRED_PREFIX));
    assert!(rows("trust-ir").starts_with(crate::state::FLIP_DEFERRED_PREFIX));
    let vc_before = rows("trust-vc");
    // The owner pins trust-ir mid-wait: the next pass holds the tuple by the pin.
    report_channel_apply(
        &layout,
        &installed,
        &report(crate::TxnOutcome::Pinned(vec!["trust-ir".into()])),
        &[],
        true,
    );
    assert_eq!(
        rows("trust-ir"),
        "held \u{2014} you pinned it (`aterm pkg unpin trust-ir` lets it update)"
    );
    assert_eq!(
        rows("trust"),
        "held \u{2014} it moves with trust-ir, which you pinned (`aterm pkg unpin \
         trust-ir` lets it update)",
        "the sibling's deferred row is over"
    );
    assert_eq!(
        crate::status::read(&layout).unwrap().programs["trust"].installed_build,
        Some(4800)
    );
    assert_eq!(
        rows("trust-vc"),
        vc_before,
        "a row that was not deferred stays"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE HOLDER VERB'S GRAMMAR: one directory and at most one `--who` before the first
/// `--`, the command whole after it — its own `--` and flags included — and every
/// other shape a usage refusal that names what is wrong.
#[test]
fn the_lease_verb_parses_a_directory_a_name_and_a_whole_command() {
    let argv = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        parse_lease_args(&argv(&[
            "/p/store/trust/9/bin",
            "--who",
            "atpkg-pack ty",
            "--",
            "bash",
            "pack.sh",
            "--",
            "--no-smoke",
        ])),
        Ok(LeaseArgs {
            dir: std::path::PathBuf::from("/p/store/trust/9/bin"),
            who: Some("atpkg-pack ty".into()),
            command: argv(&["bash", "pack.sh", "--", "--no-smoke"]),
        })
    );
    let plain = parse_lease_args(&argv(&["d", "--", "make"])).unwrap();
    assert_eq!((plain.who, plain.command), (None, argv(&["make"])));
    for (bad, why) in [
        (&["d", "make"][..], "no `--`"),
        (&["d", "--"][..], "no command"),
        (&["--", "make"][..], "no toolchain directory"),
        (&["d", "e", "--", "make"][..], "unexpected argument \"e\""),
        (&["d", "--who"][..], "no `--`"),
        (&["d", "--who", "--", "make"][..], "`--who` takes the words"),
        (
            &["d", "--who", "a", "--who", "b", "--", "make"][..],
            "unexpected argument \"--who\"",
        ),
        (
            &["d", "--bogus", "--", "make"][..],
            "unexpected argument \"--bogus\"",
        ),
    ] {
        let got = parse_lease_args(&argv(bad)).expect_err("refused");
        assert!(got.contains(why), "{bad:?}: {got}");
    }
    // A blank name is no name: the command's own words name the run.
    let blank = parse_lease_args(&argv(&["d", "--who", " ", "--", "/bin/sh", "-c", "x"])).unwrap();
    assert_eq!(
        lease_who(&blank, 7, std::path::Path::new("/w")),
        "sh -c x (pid 7) \u{2014} held by `aterm pkg lease` in /w"
    );
}

/// THE HOLDER HOLDS FOR EXACTLY THE COMMAND'S LIFE (2026-09-26): while the command runs
/// the build it names is leased — by the run's own words — and the moment it has ended
/// the lease is gone, with the command's status handed back whole. Each step waits on
/// the command's own file, never on a clock.
#[test]
fn the_lease_verb_holds_the_build_while_its_command_runs() {
    let layout = temp_layout("lease-verb-held");
    let bin = layout.build_dir("trust", 9192).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let started = layout.prefix.join("started");
    let go = layout.prefix.join("go");
    let args = LeaseArgs {
        dir: bin.clone(),
        who: Some("atpkg-pack ty".into()),
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            "touch \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.02; done; exit 7".into(),
            "sh".into(),
            started.display().to_string(),
            go.display().to_string(),
        ],
    };
    let who = lease_who(&args, 42, std::path::Path::new("/w"));
    let prefix = layout.prefix.clone();
    let run = std::thread::spawn(move || {
        let mut said = Vec::new();
        let run = lease_and_run(
            Some(&prefix),
            &args,
            &who,
            crate::lease::DEFAULT_WAIT,
            &mut |l| said.push(l.to_string()),
        );
        (run, said)
    });
    // The command's own file: it is running (a backstop bound only — the file is the
    // event).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the command never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let subject = crate::lease::Subject::build("trust", 9192).unwrap();
    assert_eq!(
        crate::lease::holders(&layout.prefix, &subject),
        crate::lease::Holders::Held(vec![
            "atpkg-pack ty (pid 42) \u{2014} held by `aterm pkg lease` in /w".into()
        ]),
        "held while the command runs"
    );
    std::fs::write(&go, b"").unwrap();
    let (run, said) = run.join().unwrap();
    let LeaseRun::Ran(status) = run else {
        panic!("the command ran: {run:?}");
    };
    assert_eq!(status.code(), Some(7), "the command's own status");
    assert_eq!(crate::platform::exit_code_like(status), ExitCode::from(7));
    assert_eq!(
        crate::lease::holders(&layout.prefix, &subject),
        crate::lease::Holders::Free,
        "let go once the command has ended"
    );
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("atpkg lease: holding trust build 9192 while atpkg-pack ty runs"),
        "{said:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A directory that is not there once the lease is laid runs NOTHING — a toolchain
/// reclaimed after its caller resolved it — while one atpkg does not manage runs its
/// command with nothing held, and says so.
#[test]
fn the_lease_verb_runs_nothing_on_a_missing_build_and_holds_nothing_outside_the_store() {
    let layout = temp_layout("lease-verb-missing");
    let ran = layout.prefix.join("ran");
    let touch = |dir: std::path::PathBuf| LeaseArgs {
        dir,
        who: None,
        command: vec!["/usr/bin/touch".into(), ran.display().to_string()],
    };
    let mut said = Vec::new();
    let gone = layout.build_dir("trust", 1).join("bin");
    let run = lease_and_run(
        Some(&layout.prefix),
        &touch(gone.clone()),
        "w",
        crate::lease::DEFAULT_WAIT,
        &mut |l| said.push(l.to_string()),
    );
    assert!(
        matches!(&run, LeaseRun::Missing(m) if m.dir == gone && m.managed),
        "{run:?}"
    );
    assert!(!ran.exists(), "nothing ran");
    assert!(said.is_empty(), "{said:?}");
    let outside = layout.prefix.join("elsewhere/bin");
    std::fs::create_dir_all(&outside).unwrap();
    // Outside the prefix's `store/`, `compat/` and `rustup/`: a directory of the
    // prefix's own that no subject names is not atpkg's to hold either.
    let run = lease_and_run(
        Some(&layout.prefix),
        &touch(outside),
        "w",
        crate::lease::DEFAULT_WAIT,
        &mut |l| said.push(l.to_string()),
    );
    assert!(matches!(run, LeaseRun::Ran(s) if s.success()), "{run:?}");
    assert!(ran.exists(), "an unmanaged toolchain's command runs");
    assert!(said[0].contains("is no build atpkg manages"), "{said:?}");
    // Unmanaged and absent: a typo, not a reclaim — nothing runs, and it is told apart.
    std::fs::remove_file(&ran).unwrap();
    let nowhere = layout.prefix.join("elsewhere/nope");
    let run = lease_and_run(
        Some(&layout.prefix),
        &touch(nowhere.clone()),
        "w",
        crate::lease::DEFAULT_WAIT,
        &mut |l| said.push(l.to_string()),
    );
    assert!(
        matches!(&run, LeaseRun::Missing(m) if m.dir == nowhere && !m.managed),
        "{run:?}"
    );
    assert!(!ran.exists(), "nothing ran");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE HOLDER IS TRANSPARENT (2026-10-02): the command it runs holds every descriptor
/// its caller handed the holder — here an unflagged one above 99, a make jobserver's
/// pipe or a `3>` redirect in life — on any std. A `std` child holds its stdio alone
/// under Trust's std since SP-1, so the holder spawns through
/// `aterm_uds::spawnfd::transparent`; its exit status still comes back whole.
#[cfg(unix)]
#[test]
fn the_lease_verb_hands_its_command_every_descriptor_its_caller_gave_it() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let null = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: F_DUPFD duplicates a descriptor this test owns onto the lowest free
    // number from 100, close-on-exec clear; the new one is owned below.
    let fd = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(fd >= 100, "F_DUPFD: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` was just opened here and nothing else owns it.
    let held = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let args = LeaseArgs {
        dir: std::path::PathBuf::from("/nonexistent/toolchain/bin"),
        who: None,
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("[ -e /dev/fd/{fd} ] && exit 3; exit 0"),
        ],
    };
    let mut said = Vec::new();
    // No prefix: nothing is leased, and the command runs all the same.
    let run = lease_and_run(None, &args, "w", crate::lease::DEFAULT_WAIT, &mut |l| {
        said.push(l.to_string());
    });
    let LeaseRun::Ran(status) = run else {
        panic!("the command ran: {run:?}");
    };
    assert_eq!(status.code(), Some(3), "fd {fd} reached the command");
    assert_eq!(crate::platform::exit_code_like(status), ExitCode::from(3));
    assert!(said[0].contains("nothing was leased"), "{said:?}");
    drop(held);
}

/// `--head-watch` (design §1.6) names the door of `update <program>`: stripped at the
/// edge wherever it sits, never an operand — and a progress file is a progress sink
/// and nothing more.
#[test]
fn the_head_watch_flag_is_stripped_wherever_it_sits() {
    let argv = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut args = argv(&["update", "claude", "--head-watch", "--wait-lock", "1800"]);
    assert!(super::take_flag(&mut args, super::HEAD_WATCH_FLAG));
    assert_eq!(args, argv(&["update", "claude", "--wait-lock", "1800"]));
    let mut args = argv(&["update", "claude", "--progress-file", "/p/progress.json"]);
    assert!(!super::take_flag(&mut args, super::HEAD_WATCH_FLAG));
    assert_eq!(
        args,
        argv(&["update", "claude", "--progress-file", "/p/progress.json"]),
        "absent: untouched"
    );
}

/// "My window is gone" is a CHANGE of parent from the spawner the edge recorded
/// — never "pid 1" on its own: the macOS orphan (launchd) and the Linux
/// subreaper orphan (any other pid) both count; the spawner itself never does,
/// even when it IS init (a scheduler's job); a parent nobody can read (Windows)
/// and a spawner nobody named never count — those wait out the bound instead.
#[test]
fn an_orphaned_waiter_is_one_whose_parent_changed_from_its_spawner() {
    assert!(
        !super::is_orphaned(Some(15359), Some(15359)),
        "still the window"
    );
    assert!(super::is_orphaned(Some(15359), Some(1)), "macOS: launchd");
    assert!(
        super::is_orphaned(Some(15359), Some(4242)),
        "Linux: a subreaper, not pid 1"
    );
    assert!(
        !super::is_orphaned(Some(15359), None),
        "Windows: the window named itself but the parent cannot be read — \
         never judged gone (this was an orphan at the first contended poll)"
    );
    assert!(
        !super::is_orphaned(Some(1), Some(1)),
        "a launchd agent's unnamed job: init IS its spawner, and it is not an \
         orphan (this was a silent, instant exit 75)"
    );
    assert!(
        !super::is_orphaned(None, Some(1)),
        "no spawner named: nothing to compare against, init included"
    );
    assert!(
        !super::is_orphaned(None, Some(4242)),
        "no spawner named and not init: never on the change rule alone"
    );
    assert!(!super::is_orphaned(None, None));
    assert_eq!(super::SPAWNER_PID_ENV, "ATPKG_SPAWNER_PID");
}

/// The spawner the edge records: the pid a window names, the parent otherwise — and
/// nobody for a pass detached on purpose, whose waiter then never reads as orphaned
/// when its detaching `sh` exits (the session's one-shot pass, Phase 3).
#[test]
fn a_detached_pass_names_no_spawner_and_waits_out_its_bound() {
    let parent = || Some(4242);
    assert_eq!(super::spawner_from(Some("15359"), parent), Some(15359));
    assert_eq!(super::spawner_from(Some(" 15359\n"), parent), Some(15359));
    assert_eq!(
        super::spawner_from(None, parent),
        Some(4242),
        "unnamed: the parent"
    );
    assert_eq!(
        super::spawner_from(Some("garbage"), parent),
        Some(4242),
        "unparsable: the parent, as before"
    );
    assert_eq!(
        super::spawner_from(Some(super::SPAWNER_DETACHED), || panic!("never asked")),
        None
    );
    let detached = super::spawner_from(Some("detached"), parent);
    assert!(
        !super::is_orphaned(detached, Some(1)),
        "re-parented to launchd"
    );
    assert!(
        !super::is_orphaned(detached, Some(4242)),
        "or to a subreaper"
    );
    assert!(
        super::is_orphaned(super::spawner_from(None, || Some(777)), Some(1)),
        "the negative control: an unnamed `sh` parent that exits orphans its waiter"
    );
}

/// ONE SENTENCE PER MARKER on a person's terminal, and none for the window's records
/// or for a fact a sentence beside it already says. The size group the window slices
/// off the starting lines survives into the sentence.
#[test]
fn a_marker_reads_as_a_sentence_on_a_terminal() {
    let said = |line: &str| human_marker(line);
    assert_eq!(
        said(
            &net_announcement(
                Consent::InstallingAterm,
                &["ay".into(), "trust".into()],
                SignedSizes {
                    download: Some(2 << 30),
                    disk: Some(6 << 30),
                },
            )
            .unwrap()
        )
        .as_deref(),
        Some(
            "Downloading 2 ALab programs (~2.0 GiB download, ~6.0 GiB on disk)\u{2026} To \
             remove them later: aterm pkg uninstall --all"
        )
    );
    assert_eq!(
        said(&format!("atpkg: {NET_INSTALLED_MARKER}ay, trust")).as_deref(),
        Some("Installed: ay, trust.")
    );
    assert_eq!(
        said(&format!(
            "atpkg: {SEED_DONE_MARKER}the pass finished; 12 program(s) are installed"
        ))
        .as_deref(),
        Some("Done \u{2014} 12 ALab programs installed.")
    );
    assert_eq!(
        said(&format!(
            "atpkg: {NET_FAILED_MARKER}network provisioning installed nothing \u{2014} see \
             the lines above for each program's reason"
        ))
        .as_deref(),
        Some(
            "Nothing was installed \u{2014} the lines above say why (details: aterm pkg \
             doctor)."
        )
    );
    let unserved = unserved_sentence("x86_64-apple-darwin", &["aarch64-apple-darwin".to_string()]);
    assert_eq!(
        unserved,
        "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
         \u{2014} it builds for aarch64-apple-darwin; nothing installs here until one is \
         published"
    );
    assert_eq!(
        unserved_sentence("x86_64-apple-darwin", &[]),
        "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
         \u{2014} nothing installs here until one is published"
    );
    assert!(seed_unusable_is_informational(&unserved));
    assert_eq!(
        said(&format!("atpkg: {SEED_UNUSABLE_MARKER}{unserved}")).as_deref(),
        Some("No ALab build is published for this machine's processor yet.")
    );
    assert_eq!(
        said(&format!(
            "atpkg: {}",
            machine_settings_line(&[spotlight_entry(3)])
        ))
        .as_deref(),
        Some("Changed this Mac's settings: hid 3 Rust build folder(s) from Spotlight.")
    );
    // Universal Control's change has its own line beside the marker, with the revert.
    let both = format!(
        "atpkg: {}",
        machine_settings_line(&[
            crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string(),
            spotlight_entry(3)
        ])
    );
    assert_eq!(
        said(&both).as_deref(),
        Some("Changed this Mac's settings: hid 3 Rust build folder(s) from Spotlight.")
    );
    let alone = format!(
        "atpkg: {}",
        machine_settings_line(&[crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()])
    );
    assert_eq!(said(&alone), None);
    // `seed-busy:` joined the silent set on 2026-09-25: the sentence beside it — the
    // intercept's epilogue, or the typed lane's timed-out sentence — already says it.
    for silent in [
        LOCK_ACQUIRED_MARKER,
        GROUP_ABORTED_MARKER,
        SHADOWED_MARKER,
        MANAGED_CURRENT_MARKER,
        MACHINE_STATE_MARKER,
        SEED_BUSY_MARKER,
    ] {
        assert_eq!(said(&format!("atpkg: {silent}x")), None, "{silent}");
    }
    assert_eq!(
        said("atpkg: not a marker").as_deref(),
        Some("atpkg: not a marker")
    );
    // Every marker the ledger lists is a sentence — but `seed-busy:`, silent above — and
    // none leaks its raw prefix.
    for (marker, _) in announcement::MARKERS {
        if *marker == SEED_BUSY_MARKER {
            continue;
        }
        let shown = said(&format!("atpkg: {marker}3 things (~1 GiB download)"));
        assert!(
            shown
                .as_deref()
                .is_some_and(|s| !s.contains(marker.trim_end())),
            "{marker}: {shown:?}"
        );
    }
}

/// An install says what can now be typed, briefly — never thirty names or the aliases.
#[test]
fn an_install_names_a_few_commands_not_every_shim() {
    let laid = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        installed_line(
            "trust",
            9192,
            &laid(&[
                "alab-targo",
                "cargo-trust-vc",
                "targo",
                "tippy",
                "trust",
                "trustc"
            ])
        ),
        "installed trust 9192 — 5 commands on your PATH (trust, cargo-trust-vc, targo, …)"
    );
    assert_eq!(
        installed_line("ay", 8256, &laid(&["alab-ay", "ay"])),
        "installed ay 8256 — `ay` is on your PATH"
    );
    assert_eq!(installed_line("x", 1, &[]), "installed x 1");
}

/// The one line an update ends with.
#[test]
fn a_channel_apply_ends_with_one_summary_line() {
    let names = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(channel_apply_summary(&[], &[], false, true), None);
    assert_eq!(
        channel_apply_summary(&names(&["ay", "clean", "trust"]), &[], false, true).as_deref(),
        Some("all 3 ALab tools are up to date")
    );
    assert_eq!(
        channel_apply_summary(&names(&["ay"]), &[], false, true).as_deref(),
        Some("ay is up to date")
    );
    assert_eq!(
        channel_apply_summary(&names(&["ay", "clean"]), &[], true, true).as_deref(),
        Some("2 ALab tools are up to date"),
        "another outcome said its own line: not \"all\""
    );
    let group = names(&["trust", "trust-cg", "trust-ir", "trust-vc"]);
    assert_eq!(
        channel_apply_summary(&group, &[], false, false).as_deref(),
        Some("trust, trust-cg, trust-ir, trust-vc are up to date"),
        "one program's group names its members, never a count that reads as the toolset"
    );
    assert_eq!(
        channel_apply_summary(&names(&["ay"]), &[], true, true).as_deref(),
        Some("ay is up to date")
    );
    let moved = vec![
        ("trust".to_string(), Some(9178), 9192),
        ("trust-cg".to_string(), Some(9178), 9192),
        ("ty".to_string(), None, 40),
    ];
    assert_eq!(
        channel_apply_summary(&names(&["ay"]), &moved, false, true).as_deref(),
        Some(
            "updated trust, trust-cg 9178 \u{2192} 9192; ty 40 (new); the rest are up to \
             date"
        )
    );
    assert_eq!(
        channel_apply_summary(&[], &moved[..1], false, true).as_deref(),
        Some("updated trust 9178 \u{2192} 9192")
    );
}

use super::*;
use std::os::unix::fs::PermissionsExt;

/// The `install` argv: one operand, or `--default-set`; a second operand is a usage
/// error before any store is touched, and so is a flag-shaped one — `--elevate=…`
/// among them since the OS-installer door went (2026-09-24, design §5.3(b)), which
/// the dispatch edge no longer lets through after the verb either.
#[test]
fn install_takes_one_operand_and_refuses_the_retired_elevate_flag() {
    let two = std::process::ExitCode::from(2);
    assert_eq!(cmd_install_argv(&["ay".to_string(), "ny".to_string()]), two);
    for mode in ["sudo", "osascript", "never"] {
        assert_eq!(
            cmd_install_argv(&[format!("--elevate={mode}")]),
            two,
            "{mode}"
        );
    }
    let set = vec!["--default-set".to_string()];
    assert_eq!(parse_install_argv(&set), Ok(Some(&set[0])));
    let (_, allowed) = NAME_TAKING_VERBS
        .iter()
        .find(|(v, _)| *v == "install")
        .unwrap();
    assert_eq!(*allowed, &["--default-set"][..]);
}

/// An unattended update must not undo a deliberate uninstall. The coherence
/// rule ("a group with any installed member is applied whole, missing siblings
/// pulled in") re-downloaded ~3.2 GB of `trust` on the next tick after the user
/// removed it (2026-08-20 round-8 audit).
#[test]
fn a_removed_program_is_read_from_the_layout_by_every_lane() {
    // The prefix goes through the REAL `store::resolve`, so it must be one the
    // chain check admits: strictly under the home directory, a private (0700) real
    // directory. The process temp dir (`/var/folders/…` on macOS) is neither under
    // the home directory nor root-owned, so `vet_prefix` fell back to the DEFAULT
    // prefix — and this test then wrote `trust` into the real store's removed
    // record where that prefix existed, or died on a missing directory where it
    // did not.
    let home = aterm_types::dirs::home_dir().expect("a home directory");
    let root = home.join(format!(".atpkg-test-removed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let layout = crate::store::resolve(Some(&root)).expect("layout");
    assert_eq!(
        layout.prefix, root,
        "PRECONDITION: the vet admits the configured prefix — a fallback here would \
         aim this test at the real store"
    );
    std::fs::write(layout.removed(), "# deliberate\ntrust\n\n").unwrap();
    let removed = layout.removed_programs();
    assert!(
        removed.contains("trust"),
        "the durable record is the answer"
    );
    assert_eq!(removed.len(), 1, "comments and blanks are not programs");
    // The CLI reader and the flow reader must be the same answer, or the
    // unattended lane disagrees with the verb the user ran.
    assert_eq!(removed, removed_programs(&layout));
    let _ = std::fs::remove_dir_all(&root);
}

/// EVERY VERB TABLE AGREES WITH THE DISPATCH. [`VERBS`] must name EXACTLY the verbs
/// `main_entry`'s match dispatches — no more (advertising a verb that no longer
/// exists) and no fewer (a working verb the hint never mentions) — and every other
/// table keyed by verb is held to it: [`VERB_TIERS`] partitions it, the tier block
/// renders each verb once, [`VERB_USAGE`] covers it both ways, and a usage line
/// names every flag its verb accepts.
///
/// Derived from `cli.rs`'s own source rather than from a second hand-kept
/// list, because a second list is the thing that rotted. `sync` and `seed`
/// were each advertised for months after deletion; both would have failed
/// this the day their arm was removed.
#[test]
fn verb_hint_matches_dispatch() {
    let src = include_str!("cli.rs");
    // The dispatch arms, in source order: `        Some("<verb>") =>`.
    let dispatched: Vec<&str> = src
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Some(\""))
        .filter_map(|rest| rest.split_once("\")"))
        .filter(|(_, tail)| tail.trim_start().starts_with("=>"))
        .map(|(verb, _)| verb)
        .collect();

    // Non-vacuity: if the extraction ever stops matching the source shape it
    // would silently compare two empty lists and pass.
    assert!(
        dispatched.len() > 10,
        "extracted only {} arm(s) — the scraper stopped matching the match block, \
         so this test was about to pass vacuously: {dispatched:?}",
        dispatched.len()
    );

    assert_eq!(
        dispatched, VERBS,
        "the unknown-verb hint and the dispatch table disagree.\n  \
         dispatched: {dispatched:?}\n  advertised: {VERBS:?}\n\
         A verb in `advertised` but not `dispatched` tells the user to run \
         something that only reprints this error."
    );

    // [`VERB_TIERS`] is an EXACT partition of [`VERBS`]: every dispatched verb in
    // exactly one tier, and no tier naming a verb that does not dispatch. A verb with
    // no tier would still parse but vanish from bare `atpkg`, `--help` and the
    // unknown-verb hint at once.
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (tier, verbs) in VERB_TIERS {
        for v in *verbs {
            assert!(
                seen.insert(v),
                "{v} appears in more than one tier (second: {tier}) — a verb has ONE home"
            );
            assert!(
                VERBS.contains(v),
                "{v} is tiered ({tier}) but not dispatched — the tiers advertise it \
                 while the match rejects it"
            );
        }
    }
    for v in VERBS {
        assert!(
            seen.contains(v),
            "{v} dispatches but has no tier — it would vanish from every human surface"
        );
    }

    // The shared tier block renders each advertised verb exactly once as a word —
    // `status` folded into `doctor (or: status)` rather than dropped.
    let block = verb_tier_lines().join("\n");
    for v in VERBS {
        let count = block
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .filter(|w| w == v)
            .count();
        assert_eq!(
            count, 1,
            "{v} must appear exactly once in the tier block:\n{block}"
        );
    }
    assert!(
        block.contains("doctor (or: status)"),
        "status renders attached to doctor, not as a sibling:\n{block}"
    );

    // Every dispatchable verb can say what it accepts (an audit found 35 of 55 CLI
    // verbs unable to), and the usage table names no verb that does not dispatch.
    for verb in VERBS {
        assert!(
            verb_usage(verb).is_some(),
            "verb `{verb}` has no VERB_USAGE entry, so `atpkg {verb} --help` \
             cannot say what it accepts"
        );
    }
    for (name, _) in VERB_USAGE {
        assert!(
            VERBS.contains(name),
            "VERB_USAGE names `{name}`, which is not a dispatchable verb"
        );
    }

    // HELP AND THE NAME-OPERAND GATE MUST NOT DRIFT: a flag [`NAME_TAKING_VERBS`]
    // accepts but the usage never names is accepted-but-undocumented, the state
    // `--elevate` occupied when an audit concluded atpkg required sudo. The option
    // NAME is checked, not the whole spelling: a usage line may collapse values into
    // one `--opt=a|b|c`.
    for (verb, allowed) in NAME_TAKING_VERBS {
        let Some(usage) = verb_usage(verb) else {
            panic!("{verb} takes flags but has no usage line");
        };
        for flag in *allowed {
            let name = flag.split('=').next().unwrap_or(flag);
            assert!(
                usage.contains(name),
                "`atpkg {verb}` accepts {flag} but its usage never names {name}:\n  {usage}"
            );
        }
    }

    // The visible option parsers outside the name-operand gate name every accepted
    // flag too. `--progress-file` and `--wait-lock` are omitted deliberately: hidden
    // machine channels the GUI adds to its spawns, stripped before verb dispatch.
    for (verb, flags) in [
        ("list", &["--porcelain"][..]),
        ("relocate", &["--sign", "--advisory"][..]),
        // Both of `noindex`'s flags are subcommand-specific, which is precisely why
        // the usage line has to name them: a reader who cannot see that `--depth`
        // belongs to `scan` learns it from a usage error instead.
        ("noindex", &["--depth", "--dry-run", "--all"][..]),
    ] {
        let usage = verb_usage(verb).expect("visible option parser has usage");
        for flag in flags {
            assert!(
                usage.contains(flag),
                "`atpkg {verb}` accepts {flag} but its usage omits it:\n  {usage}"
            );
        }
    }
}

/// `doctor --json` swallowed the flag and exited 0 with the human report
/// (audit D-12). A zero-arity verb refuses ANY argument at the usage exit,
/// naming the verb, the argument and the arity — while `--help` still
/// answers with usage via the dispatch edge, and the bare forms run.
///
/// `repair` joined the three on 2026-09-16. It was dispatched as a bare
/// `run_repair(layout())`, so `aterm pkg repair --dry-run` — or any other flag
/// or operand — was swallowed while the verb rewrote the user's shell wiring
/// and re-laid every shim at exit 0. The helper had always refused; what the
/// arm lacked was the CALL, which is why the scan below is part of this test.
#[test]
fn zero_arity_verbs_refuse_arguments_instead_of_swallowing_them() {
    const ZERO_ARITY: [&str; 4] = ["doctor", "status", "gc", "repair"];
    for verb in ZERO_ARITY {
        let rest = vec!["--json".to_string()];
        let code = zero_arity(&rest, verb).expect("an argument must be refused");
        assert_eq!(code, ExitCode::from(2), "{verb} refusal is the usage exit");
        assert!(
            zero_arity(&[], verb).is_none(),
            "bare `{verb}` must proceed"
        );
    }
    // The doctor takes `--verbose` and nothing else.
    for verb in ["doctor", "status"] {
        let arg = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            doctor_detail(&arg(&["--json"]), verb).err(),
            Some(ExitCode::from(2))
        );
        assert_eq!(
            doctor_detail(&arg(&["--verbose", "x"]), verb).err(),
            Some(ExitCode::from(2))
        );
        assert_eq!(
            doctor_detail(&arg(&["-v"]), verb).ok(),
            Some(crate::doctor::Detail::Everything)
        );
        assert_eq!(
            doctor_detail(&[], verb).ok(),
            Some(crate::doctor::Detail::Problems)
        );
    }
    // NON-VACUITY, and the half that actually regressed: the helper refuses for
    // ANY verb name, so the loop above says nothing about what a user's argv
    // reaches. Each of these verbs' dispatch arm must route through it. Read from
    // the production half, so this test's own text can never be the evidence.
    let production = production_half(include_str!("cli.rs"));
    for verb in ZERO_ARITY {
        let head = format!("Some(\"{verb}\") =>");
        let start = production
            .find(&head)
            .unwrap_or_else(|| panic!("`{verb}` must have a dispatch arm"));
        let rest = &production[start + head.len()..];
        // The arm ends where the next one begins — match arms sit at one indent
        // inside `main_entry`'s `match verb`.
        let arm = &rest[..rest.find("\n        Some(\"").unwrap_or(rest.len())];
        // The doctor's one flag is parsed by `doctor_detail`, which refuses every
        // other argument the same way.
        assert!(
            arm.contains(&format!("zero_arity(&args[1..], \"{verb}\")"))
                || arm.contains(&format!("doctor_detail(&args[1..], \"{verb}\")")),
            "`{verb}` dispatches without the arity guard, so an argument it takes \
             none of is swallowed at exit 0 — on a mutator that means the store and \
             the rc wiring change under a flag the user believes was honoured:{arm}"
        );
    }
    // `seed` is zero-arity too and refuses from inside its own fn rather than at the
    // dispatch arm, so the roster above must not be read as "the only verbs that may
    // not swallow". Pinned so that check cannot quietly go the way `repair`'s did.
    let seed_at = production.find("\nfn cmd_seed(").expect("the seed verb");
    let seed = &production[seed_at..];
    let seed = &seed[..seed[1..].find("\nfn ").map_or(seed.len(), |i| i + 1)];
    assert!(
        seed.contains("zero_arity(rest, \"seed\")"),
        "cmd_seed must still refuse the arguments it takes none of"
    );
}

/// A one-name verb refuses a second name instead of acting on the first
/// (`uninstall ty ay clean` removed `ty` alone at exit 0; measured on m3
/// 2026-09-23). The list is pinned to the CODE both ways: each listed verb's
/// dispatch arm reads `args.get(1)` and nothing after it, and every other
/// name-taking verb hands its arm the whole tail — so a verb that grows a
/// multi-name grammar, or a new one-name verb, cannot go unnoticed.
#[test]
fn one_operand_verbs_refuse_a_second_name_instead_of_dropping_it() {
    let two = vec!["ta".to_string(), "tb".to_string()];
    for verb in ONE_OPERAND_VERBS {
        assert_eq!(
            at_most_one_operand(&two, verb),
            Some(ExitCode::from(2)),
            "`{verb} ta tb` is the usage exit"
        );
        assert!(
            at_most_one_operand(&two[..1], verb).is_none(),
            "`{verb} ta` runs"
        );
        assert!(
            at_most_one_operand(&[], verb).is_none(),
            "bare `{verb}` runs"
        );
    }
    // The multi-name grammars are not this guard's to refuse.
    for verb in ["install", "link", "refresh", "run"] {
        assert!(at_most_one_operand(&two, verb).is_none(), "{verb}");
    }
    let production = production_half(include_str!("cli.rs"));
    let arm_of = |verb: &str| -> String {
        let head = format!("Some(\"{verb}\") =>");
        let start = production
            .find(&head)
            .unwrap_or_else(|| panic!("`{verb}` must have a dispatch arm"));
        let rest = &production[start + head.len()..];
        rest[..rest.find("\n        Some(\"").unwrap_or(rest.len())].to_string()
    };
    for (verb, _) in NAME_TAKING_VERBS {
        let arm = arm_of(verb);
        let one = arm.contains("args.get(1)") && !arm.contains("args[1..]");
        assert_eq!(
            ONE_OPERAND_VERBS.contains(verb),
            one,
            "`{verb}`: ONE_OPERAND_VERBS must list exactly the name-taking verbs whose \
             arm reads one operand — listed but multi-name refuses a real grammar, \
             one-name but unlisted swallows the tail:{arm}"
        );
    }
    // NON-VACUITY: the edge calls the guard, before the store lock.
    let call = production
        .find("at_most_one_operand(&args[1..], v)")
        .expect("the dispatch edge must call the guard");
    let lock = production
        .find("let _store_lock = if verb.is_some_and(verb_mutates_store)")
        .expect("the store-lock edge");
    assert!(call < lock, "the refusal must run before the store lock");
}

/// `help <x>` for an `x` that is not a verb must FAIL, not print the roster
/// at exit 0. Three byte-identical pages at rc 0 for `install`, `gc` and
/// `zzz-total-nonsense` is what made this a false success: a page of text
/// after `help <x>` reads as the help for x (audit D-C).
#[test]
fn help_for_an_unknown_topic_is_a_usage_error_not_a_roster() {
    assert_eq!(
        cmd_verb_help("zzz-total-nonsense"),
        ExitCode::from(2),
        "an unknown topic must exit 2"
    );
    // A real verb still answers, at 0.
    assert_eq!(
        cmd_verb_help("install"),
        ExitCode::SUCCESS,
        "`install` is a verb; its help is a success"
    );
    // Every advertised verb having a usage line is `verb_hint_matches_dispatch`'s.
}

/// The suggestion engine: a one-slip typo gets the fix, gibberish gets silence —
/// a far-fetched guess erodes trust in the near ones.
#[test]
fn did_you_mean_suggests_and_gives_up() {
    assert_eq!(did_you_mean("instal"), Some("install"));
    assert_eq!(did_you_mean("udpate"), Some("update"));
    // The newest verb is in the roster, so a one-slip typo of it gets the fix too.
    assert_eq!(did_you_mean("noindx"), Some("noindex"));
    assert_eq!(did_you_mean("frobnicate"), None);
}

/// The bare posture's script contract: the enabled HEAD never moves (counts ride
/// after the em dash), the disabled line is byte-exact, and the tail names exactly
/// one `next:` act.
#[test]
fn bare_posture_head_is_stable() {
    for counts in [None, Some((0, 0)), Some((10, 10))] {
        let lines = bare_lines(counts);
        assert!(
            lines[0].starts_with("atpkg: enabled (root key pinned)"),
            "the stable head leads: {:?}",
            lines[0]
        );
        let nexts = lines
            .iter()
            .filter(|l| l.starts_with("atpkg: next:"))
            .count();
        assert_eq!(
            nexts,
            usize::from(counts.is_some()),
            "one next act with a store, none without: {lines:?}"
        );
    }
    // Fresh machine: the one act is the install, and it says what it does.
    assert!(
        bare_lines(Some((0, 0)))
            .last()
            .unwrap()
            .contains("aterm pkg install --default-set"),
        "an empty store's next act is the whole-set install"
    );
}

/// NO DEAD ENDS: every empty/negative answer carries a `aterm pkg …` act the user
/// can paste — the fact alone ("ty is not installed") always raises exactly one
/// follow-up question, and the first hour should never need a second guess.
#[test]
fn empty_answers_name_a_next_command() {
    let family: Vec<String> = vec![
        EMPTY_LIST_HUMAN.to_string(),
        EMPTY_UPDATE.to_string(),
        EMPTY_VERIFY.to_string(),
        SEED_FOLLOW_ON.to_string(),
        not_installed_fix("ty"),
    ];
    for answer in &family {
        assert!(
            answer.contains("aterm pkg "),
            "an empty/negative answer must name its next command: {answer}"
        );
    }
}

/// The piped `list` form is a SCRIPT CONTRACT: `program\tbuild\tnotes`, headerless,
/// ascending builds with superseded rows before the live one — byte-identical to
/// every earlier release, dev-link and pin notes included.
/// A program DEV-LINKED FROM THE START has no store build, and `list` used to omit
/// it entirely — on a Linux box with the Trust stage2 dev-linked, `atpkg list`
/// showed only claude and codex (2026-09-14). It gets a row in both forms, with `-`
/// for the build it does not have, and counts as one program and one dev-link.
#[test]
fn a_program_dev_linked_from_the_start_is_listed_without_a_store_build() {
    let installed: Vec<(String, u64)> = vec![("claude".to_string(), 7)];
    let linked_only = vec!["trust".to_string()];
    let live: std::collections::BTreeMap<String, u64> = [("claude".to_string(), 7)].into();
    let linked: std::collections::BTreeMap<String, Option<std::path::PathBuf>> = [(
        "trust".to_string(),
        Some(std::path::PathBuf::from("/h/trust/build/host/stage2")),
    )]
    .into();
    let pinned = std::collections::BTreeSet::new();

    let porcelain = list_porcelain_lines(&installed, &live, &linked, &pinned);
    assert_eq!(
        porcelain[1], "trust\t-\tDEV-LINKED -> /h/trust/build/host/stage2",
        "{porcelain:?}"
    );
    assert_eq!(
        porcelain[0], "claude\t7\tlive",
        "installed rows are byte-identical to before: {porcelain:?}"
    );

    let human = list_human_lines(&installed, &linked_only, &live, &linked, &pinned);
    assert!(
        human[0].starts_with("atpkg: 2 program(s) — 1 store builds live, 1 dev-linked"),
        "{human:?}"
    );
    let row = human
        .iter()
        .find(|l| l.contains("trust"))
        .expect("a trust row");
    assert!(
        row.contains("DEV-LINKED -> /h/trust/build/host/stage2"),
        "{row}"
    );
    assert!(
        row.contains("(no store build — linked from the start)"),
        "{row}"
    );
    assert!(
        !row.contains("shadowed"),
        "nothing is shadowed when there is no store build: {row}"
    );
}

#[test]
fn list_porcelain_is_stable() {
    let installed = vec![
        ("ay".to_string(), 7987),
        ("ay".to_string(), 8256),
        ("clean".to_string(), 7345),
    ];
    let live: std::collections::BTreeMap<String, u64> =
        [("ay".to_string(), 8256), ("clean".to_string(), 7345)].into();
    let linked = std::collections::BTreeMap::new();
    let mut pinned = std::collections::BTreeSet::new();
    pinned.insert("clean".to_string());
    assert_eq!(
        list_porcelain_lines(&installed, &live, &linked, &pinned),
        vec![
            "ay\t7987\tsuperseded (kept for rollback)".to_string(),
            "ay\t8256\tlive".to_string(),
            "clean\t7345\tlive  pinned".to_string(),
        ],
    );
}

#[test]
fn list_includes_programs_provisioned_only_through_development_links() {
    let linked = [(
        "ay".to_string(),
        Some(std::path::PathBuf::from("/local/seal")),
    )]
    .into();
    let live = std::collections::BTreeMap::new();
    let pinned = std::collections::BTreeSet::new();
    assert_eq!(
        list_porcelain_lines(&[], &live, &linked, &pinned),
        vec!["ay\t-\tDEV-LINKED -> /local/seal"],
    );
    let human = list_human_lines(&[], &[], &live, &linked, &pinned);
    assert_eq!(
        human[0],
        "atpkg: 1 program(s) — 0 store builds live, 1 dev-linked"
    );
    assert!(human[1].contains("DEV-LINKED -> /local/seal"));
    assert!(
        !human
            .iter()
            .any(|line| line.contains("NO LIVE BUILD") || line.contains("next"))
    );

    let installed = vec![("clean".to_string(), 12), ("clean".to_string(), 13)];
    let live = [("clean".to_string(), 13)].into();
    assert_eq!(
        list_porcelain_lines(&installed, &live, &linked, &pinned),
        vec![
            "ay\t-\tDEV-LINKED -> /local/seal",
            "clean\t12\tsuperseded (kept for rollback)",
            "clean\t13\tlive",
        ],
        "existing signed-build rows retain their numeric identity and order",
    );
    let shadowed = [("clean".to_string(), None)].into();
    let mixed = list_porcelain_lines(&installed, &live, &shadowed, &pinned);
    assert_eq!(
        mixed.len(),
        2,
        "do not duplicate linked programs already represented by store rows"
    );
}

/// The human report leads with the verdict-shaped summary, folds superseded rows
/// into a per-program count, and appends its one `next` act only when some program
/// has no live build (counts only, no health adjective — "healthy" is doctor's word).
#[test]
fn list_human_report_summarizes_and_names_doctor_only_when_needed() {
    let installed = vec![
        ("ay".to_string(), 7987),
        ("ay".to_string(), 8256),
        ("clean".to_string(), 7345),
    ];
    let live: std::collections::BTreeMap<String, u64> =
        [("ay".to_string(), 8256), ("clean".to_string(), 7345)].into();
    let none_linked = std::collections::BTreeMap::new();
    let none_pinned = std::collections::BTreeSet::new();
    let lines = list_human_lines(&installed, &[], &live, &none_linked, &none_pinned);
    assert_eq!(lines[0], "atpkg: 2 program(s) — 2 live");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("(1 older build(s) kept for rollback)")),
        "the superseded build survives as a count: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("next")),
        "a healthy listing names no next act: {lines:?}"
    );
    // A program with builds on disk and nothing live is the row that needs doctor.
    let dead_live: std::collections::BTreeMap<String, u64> = [("ay".to_string(), 8256)].into();
    let lines = list_human_lines(&installed, &[], &dead_live, &none_linked, &none_pinned);
    assert_eq!(
        lines[0],
        "atpkg: 2 program(s) — 1 live, 1 without a live build"
    );
    assert!(
        lines.iter().any(|l| l.contains("NO LIVE BUILD")),
        "the dead program is loud: {lines:?}"
    );
    assert_eq!(
        lines.last().unwrap(),
        "atpkg: next — aterm pkg doctor",
        "exactly one next act, at the end: {lines:?}"
    );
}

/// `list` with no resolvable prefix exits 1 like every sibling read verb — it used
/// to print the error and exit 0, a silent green in an already-broken environment.
#[test]
fn list_exit_codes() {
    assert_eq!(
        format!("{:?}", run_list(None, true)),
        format!("{:?}", ExitCode::from(1)),
        "no layout is a structural failure, not a success"
    );
}

/// UNINSTALLING WHAT IS NOT THERE IS A REFUSAL, NOT A SUCCESS. This used to print
/// "uninstalled ty", exit 0, and mint a durable removed-marker that silently
/// suppressed future set-completion — a fabricated success with a hidden state
/// change. Partial-install debris (a store tree with no shims) must still clean.
#[test]
fn uninstall_not_installed_refuses() {
    let layout = temp_layout("uninstall-absent");
    assert_eq!(
        format!("{:?}", uninstall_one(&layout, "ty")),
        format!("{:?}", ExitCode::from(1)),
        "nothing bears the name, so nothing was uninstalled"
    );
    assert!(
        !removed_programs(&layout).contains("ty"),
        "no removed-marker is minted for a program that was never here"
    );
    // Partial debris: a store tree with no shims is still this verb's to clean.
    std::fs::create_dir_all(layout.build_dir("ty", 7)).unwrap();
    assert_eq!(
        format!("{:?}", uninstall_one(&layout, "ty")),
        format!("{:?}", ExitCode::SUCCESS),
        "debris cleans as before"
    );
    assert!(
        !layout.prefix.join("store").join("ty").exists(),
        "the debris really came off"
    );
    assert!(
        removed_programs(&layout).contains("ty"),
        "a real removal still records itself for the seed lane"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `uninstall --all` MAY ONLY REPORT WHAT IT ACTUALLY REMOVED.
///
/// The sweep walks the union of the live builds and EVERY status row, and
/// `ops::uninstall` returns Ok for a name with no store tree under the prefix. So a
/// system copy (`system: /opt/homebrew/bin/gh`), a row an older client wrote for an
/// OS-level install (`installed via pkg: …` — the Command Line Tools) and an agent
/// program still queued were all counted as removals, printed in `atpkg: removed …`,
/// and had their rows wiped — while the real binary stayed exactly where it was and
/// still ran. The design says the opposite
/// (`docs/DESIGN-which-copy-runs-2026-08-27.md` §S11): "system copies are never
/// touched and the output says so".
#[test]
fn uninstall_all_removes_only_what_aterm_installed() {
    let layout = temp_layout("uninstall-all-foreign");
    make_live(&layout, "ay", 18);
    let gh = crate::state::system(std::path::Path::new("/opt/homebrew/bin/gh"), None);
    for (program, build, state) in [
        ("ay", Some(18), crate::state::managed(18, 41)),
        ("gh", None, gh.clone()),
        // Retired spellings an older client wrote (design §5.3(b)/(c)).
        (
            "clt",
            None,
            "installed via pkg: /Library/Developer/CommandLineTools".to_string(),
        ),
        (
            "ty",
            None,
            "extra — not installed (opt in: aterm pkg install ty)".to_string(),
        ),
        ("codex", None, crate::state::agent_installing()),
    ] {
        record_status(
            &layout,
            program,
            crate::ProgramStatus {
                installed_build: build,
                state,
                tree_root: String::new(),
            },
            "up to date".into(),
        );
    }

    let targets = uninstall_all_targets(&layout);
    assert!(
        targets.contains("ay") && targets.contains("gh") && targets.contains("clt"),
        "precondition: the sweep walks the live build AND every row: {targets:?}"
    );
    let sweep = sweep_uninstall_targets(&layout, &targets);

    assert_eq!(
        sweep.removed,
        ["ay"],
        "only the managed copy was ever this manager's to remove"
    );
    assert_eq!(sweep.failures, 0, "and nothing failed");
    assert_eq!(
        sweep.left_alone,
        ["gh (system copy: /opt/homebrew/bin/gh)"],
        "the copy still on this machine is NAMED, never claimed as removed"
    );
    assert!(
        !program_present(&layout, "ay"),
        "the managed copy really came off"
    );

    let rows = crate::status::read(&layout)
        .expect("record present")
        .programs;
    assert_eq!(
        rows.get("gh").map(|r| r.state.as_str()),
        Some(gh.as_str()),
        "a system row survives: it is where that copy lives, and clearing it only \
         made the next pass re-discover it"
    );
    assert!(
        !rows.contains_key("ay")
            && !rows.contains_key("clt")
            && !rows.contains_key("ty")
            && !rows.contains_key("codex"),
        "a row that outlived its program still goes — it just is not a removal: {rows:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A cause is one sentence: the first line, bounded, an ellipsis when cut.
#[test]
fn clip_cause_keeps_the_first_line_bounded() {
    assert_eq!(
        clip_cause("staging failed: io: disk full\ncurl said more"),
        "staging failed: io: disk full"
    );
    assert_eq!(clip_cause("  spaced  \n"), "spaced");
    let long = "x".repeat(300);
    let clipped = clip_cause(&long);
    assert_eq!(clipped.chars().count(), 241, "{clipped}");
    assert!(clipped.ends_with('…'));
}

/// Only an AGENT program's stub passes through, and only to a foreign copy on PATH
/// outside the managed prefix: `trust` never does, and `claude` with no other copy
/// prints its state and stops as before (2026-09-15).
#[cfg(unix)]
#[test]
fn only_an_agent_program_with_a_foreign_copy_passes_through() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = temp_layout("passthrough");
    // Named for this process: the temp dir is shared, and a second run of this suite
    // beside this one removed a fixed `foreign-bin` under this run's feet.
    let foreign = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-foreign-bin-{}", std::process::id()));
    std::fs::create_dir_all(&foreign).unwrap();
    for name in ["claude", "trust"] {
        let exe = foreign.join(name);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths([l.agents_dir(), foreign.clone(), l.bin_dir()]).unwrap();
    assert_eq!(
        pending_passthrough(&l, "claude", Some(&path)),
        Some(foreign.join("claude")),
        "an agent program runs the user's own copy meanwhile"
    );
    assert_eq!(
        pending_passthrough(&l, "trust", Some(&path)),
        None,
        "a toolchain program's stub never passes through"
    );
    let managed_only = std::env::join_paths([l.agents_dir(), l.bin_dir()]).unwrap();
    assert_eq!(
        pending_passthrough(&l, "claude", Some(&managed_only)),
        None,
        "no foreign copy: the stub prints its state and stops"
    );
    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// `atpkg repair` names each rc it edited. It used to print one "shell integration
/// rewritten" line whether it had re-laid a block the user deleted, failed to write the
/// rc, or found no rc at all — so re-laying a user's opt-out went unannounced. Pinned over
/// real passes on a synthetic home: a re-laid opt-out is named with the way back out, an
/// rc already wired gets no line, and every refusal says what happened. No line claims
/// "rc wiring" wholesale.
#[test]
fn repair_names_the_rc_it_relaid_over_an_opt_out() {
    use crate::hooks::{HookPass, RcOutcome, RcWiring};
    let layout = temp_layout("repair-rc");
    let home = crate::testdir::dir("atpkg-main-repair-rc-home-");
    let zshrc = home.join(".zshrc");
    let mine = "export FOO=1\n";
    std::fs::write(&zshrc, mine).unwrap();
    let rewritten = "repair: ~/.aterm/shell.d hooks in place".to_string();

    // atpkg wires ~/.zshrc once (which records it in the ledger), and the user
    // deletes the block afterwards — the opt-out, exactly as the block words it.
    let first = crate::hooks::pass_at(&layout, &home, RcWiring::HonorOptOut);
    assert_eq!(first.rc(), [(".zshrc", RcOutcome::Appended)]);
    std::fs::write(&zshrc, mine).unwrap();

    let unattended = crate::hooks::pass_at(&layout, &home, RcWiring::HonorOptOut);
    assert_eq!(unattended.rc(), [(".zshrc", RcOutcome::OptOutKept)]);
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), mine);

    let repaired = crate::hooks::pass_at(&layout, &home, RcWiring::Rewire);
    assert_eq!(repaired.rc(), [(".zshrc", RcOutcome::RelaidOverOptOut)]);
    assert!(
        std::fs::read_to_string(&zshrc)
            .unwrap()
            .contains(crate::hooks::RC_BEGIN)
    );
    let lines = super::repair_hook_lines(&repaired);
    assert_eq!(lines.first(), Some(&rewritten), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("re-laid atpkg's block in ~/.zshrc")
                && l.contains("you had deleted it")
                && l.contains("delete it again to opt out")),
        "the re-laid opt-out is named, with the way back out: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("rc wiring")),
        "no wholesale claim: {lines:?}"
    );

    // A second repair finds the block in place: nothing happened to the rc, no line.
    let again = crate::hooks::pass_at(&layout, &home, RcWiring::Rewire);
    assert_eq!(again.rc(), [(".zshrc", RcOutcome::AlreadyWired)]);
    assert_eq!(super::repair_hook_lines(&again), vec![rewritten.clone()]);

    // An rc linked into a read-only directory (a home-manager store's shape): the
    // write is refused, the target is untouched, and repair says so.
    // Non-root only: root can write a 0555 directory, so the refusal never happens.
    if crate::platform::our_uid() != 0 {
        let store = home.join("ro-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("zshrc"), mine).unwrap();
        std::fs::remove_file(&zshrc).unwrap();
        std::os::unix::fs::symlink(store.join("zshrc"), &zshrc).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        let ro = crate::hooks::pass_at(&layout, &home, RcWiring::Rewire);
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ro.rc(), [(".zshrc", RcOutcome::WriteFailed)]);
        assert_eq!(std::fs::read_to_string(store.join("zshrc")).unwrap(), mine);
        let lines = super::repair_hook_lines(&ro);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("~/.zshrc could NOT be rewritten")),
            "{lines:?}"
        );
    }

    // No rc at all: said, not implied — and the roster is named, not summarised.
    std::fs::remove_file(&zshrc).unwrap();
    let none = crate::hooks::pass_at(&layout, &home, RcWiring::Rewire);
    assert!(none.rc().is_empty());
    assert_eq!(
        super::repair_hook_lines(&none),
        vec![
            rewritten,
            "repair: none of ~/.zshrc, ~/.bashrc, ~/.bash_profile, \
             ~/.config/fish/config.fish exists, so no rc was wired (atpkg never \
             creates one)"
                .to_string()
        ]
    );

    // ~/.aterm refused (a symlink): nothing touched, and the one line says so.
    let refused = home.join("refused-home");
    std::fs::create_dir_all(refused.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(refused.join("elsewhere"), refused.join(".aterm")).unwrap();
    std::fs::write(refused.join(".zshrc"), mine).unwrap();
    let pass = crate::hooks::pass_at(&layout, &refused, RcWiring::Rewire);
    assert_eq!(pass, HookPass::NotHardened);
    let lines = super::repair_hook_lines(&pass);
    assert!(
        lines.len() == 1 && lines[0].contains("no rc was read or written"),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(refused.join(".zshrc")).unwrap(),
        mine
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-main-{label}-"))
}

/// A parsed index shaped by hand: `(name, extra, system)` per program. No channels —
/// the set-planning helpers under test read `[programs]` only.
fn index_of(programs: &[(&str, Option<&str>)]) -> crate::manifest::Index {
    crate::manifest::Index {
        schema: 2,
        index_build: 1,
        generated_at: String::new(),
        valid_until: String::new(),
        machine_id: None,
        roster_seq: None,
        programs: programs
            .iter()
            .map(|(name, system)| {
                (
                    (*name).to_string(),
                    crate::manifest::Program {
                        repo: (*name).to_string(),
                        policy: String::new(),
                        coherence_group: None,
                        system: system.map(str::to_string),
                        unavailable_hint: None,
                        requires: vec![],
                    },
                )
            })
            .collect(),
        channels: vec![],
    }
}

/// The pass lists `bin/` once, not once per installed program: the reading half shares one
/// [`crate::ops::BinScan`] taken at the top of [`reconcile_aliases`]. The one residual
/// per-program listing is `prune_stale_shims`, deliberately — it deletes files on the
/// user's `PATH`, so it reads the directory itself at the moment it decides. Pinning the
/// cost at `1 + N`, not `1 + 2N`, catches an edit that prunes from a stale snapshot or puts
/// the shared scan back inside the loop.
#[test]
fn the_alias_pass_lists_bin_once_and_per_program_only_where_it_deletes() {
    let layout = temp_layout("alias-pass-scans");
    let programs = ["pa", "pb", "pc", "pd", "pe", "pf"];
    for p in programs {
        let build = layout.build_dir(p, 7);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join(p), b"#!/bin/true\n").unwrap();
        crate::activate::install_shims(
            &layout,
            &build,
            std::slice::from_ref(&p.to_string()),
            crate::activate::Aliases::Alab,
        )
        .unwrap();
    }
    let rows: Vec<(&str, Option<&str>)> = programs.iter().map(|p| (*p, None)).collect();
    let index = index_of(&rows);
    // Measure the steady state: the first pass lays whatever aliases are missing, the
    // second is the six-hourly tick that finds everything already right and writes
    // nothing. That second pass is the one whose reads are being counted.
    reconcile_aliases(&layout, &index);
    crate::ops::reset_bin_scans();
    reconcile_aliases(&layout, &index);
    let scans = crate::ops::bin_scans();
    assert_eq!(
        scans,
        programs.len() + 1,
        "one shared scan for the whole pass and one prune scan per program (it deletes) \
         — got {scans} for {} programs",
        programs.len()
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The shadow pass lists `bin/` once, whatever is installed. [`reconcile_shadowed`] only
/// reads, prints and records — nothing in it writes to or deletes from `bin/` — so its
/// whole cost is the one photograph, with no per-program remainder to allow for.
#[test]
fn the_shadow_pass_lists_bin_once_for_the_whole_machine() {
    let layout = temp_layout("shadow-pass-scans");
    for p in ["qa", "qb", "qc", "qd"] {
        let build = layout.build_dir(p, 3);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join(p), b"#!/bin/true\n").unwrap();
        crate::activate::install_shims(
            &layout,
            &build,
            std::slice::from_ref(&p.to_string()),
            crate::activate::Aliases::Off,
        )
        .unwrap();
    }
    crate::ops::reset_bin_scans();
    let _ = reconcile_shadowed(&layout, 1, None);
    let scans = crate::ops::bin_scans();
    assert_eq!(
        scans, 1,
        "the shadow reconcile reads `bin/` once for the whole machine — got {scans}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

fn names(v: &[&str]) -> std::collections::BTreeSet<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// The wanted set is the signed index narrowed by `[packages].exclude` — every program
/// it names, an older index's `extra` row included since that tier was retired
/// (design §5.3(c)): nothing to opt in to, and a config exclude still subtracts.
#[test]
fn the_wanted_set_is_the_index_narrowed_by_exclude() {
    let index = index_of(&[("ay", None), ("vendorx", None)]);
    let cfg = crate::config::PackagesConfig::default();
    assert_eq!(wanted_programs(&index, &cfg), names(&["ay", "vendorx"]));
    let narrowed = crate::config::PackagesConfig {
        exclude: Some(vec!["vendorx".to_string()]),
        ..Default::default()
    };
    assert_eq!(wanted_programs(&index, &narrowed), names(&["ay"]));
}

// `which <program>` for a bundle whose build exposes no command of its own name
// (`trust` → trustc/targo, `trust-mc` → trust-mc-driver) answers with the managed
// store dir and the names it DOES put on PATH; a same-named unrelated binary on PATH
// (Homebrew's p11-kit `trust`) is named as exactly that, after the truth — never as
// "the system copy" of a program it is not (2026-09-10 audit).
#[cfg(unix)]
#[test]
fn which_for_a_bundle_program_names_the_store_and_its_exposed_tools() {
    let layout = temp_layout("which-bundle");
    let build = layout.build_dir("tm", 20065);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    for exe in ["tm-driver", "tm-compiler"] {
        std::fs::write(build.join("bin").join(exe), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            build.join("bin").join(exe),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    // Only the driver is exposed — the compiler is an internal, like the real pack.
    crate::activate::install_shims(
        &layout,
        &build,
        &["tm-driver".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(&layout, &build).unwrap();
    crate::store::mark_build_ready(&build).unwrap();
    record_status(
        &layout,
        "tm",
        crate::ProgramStatus {
            installed_build: Some(20065),
            state: crate::state::managed(20065, 21),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    // A same-named UNRELATED binary on PATH (p11-kit's `trust`).
    let brew = layout.prefix.parent().unwrap().join(format!(
        "atpkg-main-which-bundle-brew-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&brew);
    std::fs::create_dir_all(&brew).unwrap();
    std::fs::write(brew.join("tm"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(brew.join("tm"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([brew.clone(), layout.bin_dir()]).unwrap();
    let line = which_line(&layout, "tm", Some(&path)).unwrap();
    let mut lines = line.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!(
            "tm → {} (bundle; exposes tm-driver — run `aterm pkg which tm-driver`) — \
             managed 20065 — pinned by index 21",
            build.display()
        )
    );
    assert_eq!(
        lines.next().unwrap(),
        format!(
            "{} on PATH is an unrelated binary, not this program",
            brew.join("tm").display()
        )
    );
    assert!(lines.next().is_none());
    // Without the impostor: one line.
    let line = which_line(&layout, "tm", Some(std::ffi::OsStr::new(""))).unwrap();
    assert_eq!(line.lines().count(), 1);
    // The exposed tool itself still answers by the ordinary shim arm.
    let line = which_line(&layout, "tm-driver", Some(std::ffi::OsStr::new(""))).unwrap();
    assert!(line.starts_with("tm-driver → "), "{line}");
    assert!(line.contains("managed 20065"), "{line}");
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&brew);
}

/// `atpkg which` for an AGENT PROGRAM (owner decision 2026-09-10): with `agents/`
/// first on `PATH` the managed copy is what runs — the canonical managed line — and a
/// SECOND line names every foreign copy it out-ranked, home-abbreviated, with the
/// version its real location spells; nothing is executed to learn it. Without the
/// agents dir on `PATH` (a shell the hook never reached) the same foreign copy is
/// reported SHADOWING in an aterm shell, and as that shell's own copy — a note — in
/// one outside aterm (2026-09-24).
#[cfg(unix)]
#[test]
fn which_for_an_agent_program_names_the_managed_copy_and_the_out_ranked_foreign_copies() {
    let layout = temp_layout("which-agent");
    let v281 = vendor_version("2.1.281").build_id();
    let build = layout.build_dir("claude", v281);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &build,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    let claude = crate::store::ToolName::new("claude").unwrap();
    assert!(layout.agent_shim(&claude).exists(), "the twin is laid");
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(v281),
            state: crate::state::vendor_managed("2.1.281", "Anthropic"),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    // Two foreign copies shaped like the real machine: Anthropic's native install
    // (`~/.local/bin/claude → …/versions/2.1.267`) and a Homebrew cask
    // (`…/Caskroom/claude-code/2.1.204/claude`), reached through a symlink each.
    let root = crate::testdir::dir("atpkg-main-which-agent-foreign-");
    let native_real = root.join("share/claude/versions/2.1.267");
    let cask_real = root.join("Caskroom/claude-code/2.1.204/claude");
    for real in [&native_real, &cask_real] {
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(real, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(real, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let local = root.join("local-bin");
    let brew = root.join("brew-bin");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&brew).unwrap();
    std::os::unix::fs::symlink(&native_real, local.join("claude")).unwrap();
    std::os::unix::fs::symlink(&cask_real, brew.join("claude")).unwrap();
    let agents_first = std::env::join_paths([
        layout.agents_dir(),
        local.clone(),
        brew.clone(),
        layout.bin_dir(),
    ])
    .unwrap();
    let line = which_line(&layout, "claude", Some(&agents_first)).unwrap();
    let mut lines = line.lines();
    // (…and, since 2026-09-19, the self-update sentence closes the managed line —
    // `crate::selfupdate`: the twin answers `claude update` with the standard
    // update — ahead of the foreign-copies line.)
    assert_eq!(
        lines.next().unwrap(),
        // agents/ first: the TWIN is the copy that runs, and it is what the sentence
        // that closes the line is about (audit, 2026-09-25 — `bin/` was named here).
        format!(
            "claude → {} → {} — managed 2.1.281 — Anthropic latest — `claude update` here \
             runs `aterm pkg update claude`",
            layout.agent_shim(&claude).display(),
            crate::which(&layout, "claude").unwrap().display()
        )
    );
    assert_eq!(
        lines.next().unwrap(),
        format!(
            "foreign copies out-ranked: {} (2.1.267), {} (2.1.204)",
            local.join("claude").display(),
            brew.join("claude").display()
        )
    );
    assert_eq!(lines.next(), None);
    // No foreign copy at all: the managed line alone.
    let managed_only = std::env::join_paths([layout.agents_dir(), layout.bin_dir()]).unwrap();
    assert_eq!(
        which_line(&layout, "claude", Some(&managed_only))
            .unwrap()
            .lines()
            .count(),
        1
    );
    // The agents dir NOT on PATH (a shell from before the install): the native
    // install ahead of bin/ runs in THAT shell — and since 2026-09-16 the answer
    // says exactly that, with the in-place remedy, instead of the machine-wide
    // `SHADOWED by` every other member gets (owner, 2026-09-16: "it is telling me
    // to open a new tab. NO! all the latest and best MUST WORK IN THE SAME TAB");
    // the foreign copies are still listed — as what they are in THIS shell, on its
    // PATH, not "out-ranked" (review, 2026-09-16) — so the picture is whole.
    //   That is an aterm shell's answer: the remedy moves agents/ first THERE.
    let no_agents = std::env::join_paths([local.clone(), layout.bin_dir()]).unwrap();
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let shadowed = which_line_in(&layout, "claude", Some(&no_agents), inside).unwrap();
    assert_eq!(
        shadowed,
        format!(
            "claude → {} — {}\nforeign copies on this shell's PATH: {} (2.1.267)",
            local.join("claude").display(),
            crate::state::agent_shadowed_in_shell(
                v281,
                &local.join("claude"),
                &layout.agents_dir(),
                false,
                &shell_remedy_command(&layout),
            ),
            local.join("claude").display()
        )
    );
    assert!(
        !shadowed.contains("here runs"),
        "the twin is not what runs in that shell, so no self-update sentence: {shadowed}"
    );
    // Outside aterm (an iTerm shell; no reroute stub laid here at all) the same PATH
    // running the user's own copy is the design (owner law, 03513b5d7; review,
    // 2026-09-24): a note, never SHADOWED — the hook remedy does nothing there, and
    // the no-hook `export PATH="<agents>:$PATH"` line would undo the law.
    let own = which_line_in(
        &layout,
        "claude",
        Some(&no_agents),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert_eq!(
        own,
        format!(
            "claude → {} — {}",
            local.join("claude").display(),
            crate::state::agent_own_copy_outside_aterm(v281, &local.join("claude"))
        )
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `which claude` in a shell whose reroute stub decides at exec time (2026-09-23): inside
/// aterm the stub takes the twin, so the line names that route — stub, twin, store —
/// with the self-update sentence the twin makes true, whatever this shell's PATH puts
/// ahead; outside aterm the stub passes through to this shell's own copy, which is the
/// design and said as such (never SHADOWED, no remedy) — and where the pass-through
/// reaches `bin/`, the managed line carries no self-update sentence, since the twin
/// that makes it true is not what runs there.
#[cfg(unix)]
#[test]
fn which_for_an_agent_program_names_the_reroute_stubs_route_at_exec_time() {
    let layout = temp_layout("which-agent-routed");
    let v281 = vendor_version("2.1.281").build_id();
    let build = layout.build_dir("claude", v281);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &build,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::reroute::lay(&layout).unwrap();
    let claude = crate::store::ToolName::new("claude").unwrap();
    let stub = crate::reroute::stub_path(&layout, "claude");
    assert!(
        crate::reroute::is_reroute_stub(&stub),
        "the twin earns a stub"
    );
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(v281),
            state: crate::state::vendor_managed("2.1.281", "Anthropic"),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    let local = layout.prefix.parent().unwrap().join(format!(
        "atpkg-main-which-agent-routed-local-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(local.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let stale = std::env::join_paths([
        crate::reroute::dir(&layout),
        local.clone(),
        layout.bin_dir(),
    ])
    .unwrap();
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let line = which_line_in(&layout, "claude", Some(&stale), inside).unwrap();
    let first = line.lines().next().unwrap();
    assert_eq!(
        first,
        format!(
            "claude → {} → {} → {} — managed 2.1.281 — Anthropic latest — `claude update` \
             here runs `aterm pkg update claude`",
            stub.display(),
            layout.agent_shim(&claude).display(),
            crate::which(&layout, "claude").unwrap().display()
        )
    );
    assert!(!line.contains("SHADOWED"), "{line}");
    // Outside aterm: the stub passes through to this shell's own copy — the design,
    // said as such, with no SHADOWED and no remedy.
    let outside = which_line_in(
        &layout,
        "claude",
        Some(&stale),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert_eq!(
        outside,
        format!(
            "claude → {} — {}",
            local.join("claude").display(),
            crate::state::agent_passed_through(v281, &stub, &local.join("claude"))
        )
    );
    // …and so is an iTerm shell's, whose PATH has no reroute dir at all (2026-09-24):
    // the stub is laid, this shell's own copy runs by design — no SHADOWED, no remedy.
    let iterm = std::env::join_paths([local.clone(), layout.bin_dir()]).unwrap();
    let line = which_line_in(
        &layout,
        "claude",
        Some(&iterm),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert_eq!(
        line,
        format!(
            "claude → {} — {}",
            local.join("claude").display(),
            crate::state::agent_own_copy_outside_aterm(v281, &local.join("claude"))
        )
    );
    // …and where the pass-through reaches `bin/` (nothing of the user's on PATH, with
    // `agents/` on it but skipped by the stub), the managed line stands WITHOUT the
    // self-update sentence: `bin/claude` carries no hand-over, and the twin that does
    // is not what runs there.
    let bin_only = std::env::join_paths([
        crate::reroute::dir(&layout),
        layout.agents_dir(),
        layout.bin_dir(),
    ])
    .unwrap();
    let line = which_line_in(
        &layout,
        "claude",
        Some(&bin_only),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert!(
        line.starts_with(&format!("claude → {} → ", layout.shim(&claude).display())),
        "{line}"
    );
    assert!(!line.contains("here runs"), "{line}");
    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The version a foreign copy's path spells: the LAST dotted-digits component, or
/// nothing — never a guess.
#[test]
fn version_from_components_reads_the_last_dotted_digits_component() {
    let v = |p: &str| version_from_components(std::path::Path::new(p));
    assert_eq!(
        v("/Users//x/.local/share/claude/versions/2.1.267").as_deref(),
        Some("2.1.267")
    );
    assert_eq!(
        v("/opt/homebrew/Caskroom/claude-code/2.1.204/claude").as_deref(),
        Some("2.1.204")
    );
    assert_eq!(
        v("/opt/homebrew/Caskroom/codex/0.151.0/bin/codex").as_deref(),
        Some("0.151.0")
    );
    assert_eq!(v("/usr/local/bin/claude"), None);
    assert_eq!(
        v("/x/1.2.3/y/4.5/claude").as_deref(),
        Some("4.5"),
        "last wins"
    );
    assert_eq!(v("/x/v1.2.3/claude"), None, "digits only");
    assert_eq!(v("/x/.hidden/claude"), None, "a dot alone is not a version");
}

/// "NEVER CHECKED" IS DATA, NEVER A LINE (Phase 2, 2026-09-22; until then the
/// read-only verbs and the pending stub printed it on stderr, and this test pinned
/// that they did): against a scratch prefix with no `status.toml` the condition holds;
/// a failed pass's record (a `*index*` error row, `updated_at` fresh) does not clear
/// it; only a pass end that reached the index ([`record_pass_end`], which `update` and
/// `install --default-set` call once) does, and a later per-program write carries the
/// stamp.
/// `aterm pkg doctor` is the one surface that reports it (`doctor::run_with`).
#[test]
fn never_checked_is_data_until_a_pass_has_succeeded() {
    let layout = temp_layout("never-checked");
    assert!(
        crate::status::never_checked(&layout),
        "no status.toml: never checked"
    );
    record_status(
        &layout,
        "*index*",
        crate::ProgramStatus {
            installed_build: None,
            state: "error: index unreachable".into(),
            tree_root: String::new(),
        },
        "update failed: index unreachable".into(),
    );
    assert!(
        crate::status::never_checked(&layout),
        "a failed pass's record is not a completed check"
    );
    record_pass_end(&layout, now_unix(), true);
    assert!(!crate::status::never_checked(&layout), "stamped");
    let back = crate::status::read(&layout).unwrap();
    assert!(!back.last_success_at.is_empty());
    // A later per-program write CARRIES the stamp rather than dropping it.
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: crate::state::managed(18, 41),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    assert_eq!(
        crate::status::read(&layout).unwrap().last_success_at,
        back.last_success_at,
        "record_status carries last_success_at"
    );
    assert!(!crate::status::never_checked(&layout));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

// `machine-settings:` is the contract line the GUI parses for its ledger entries:
// entries `; `-separated, the Spotlight count as `spotlight-noindex N dir(s)
// migrated`, Universal Control as `universal-control disabled`; nothing printed for
// an empty pass (the callers check).
#[test]
fn machine_write_failure_is_explicit_without_losing_partial_changes() {
    use crate::machine::UniversalControlOutcome;
    let spotlight = spotlight_entry(2);
    let mut entries = vec![spotlight.clone()];
    let failure =
        record_universal_control_outcome(UniversalControlOutcome::WriteFailed, &mut entries)
            .expect("a failed write must be reported even when the verb exits 0");
    assert_eq!(
        MACHINE_APPLY_FAILED_PREFIX,
        "atpkg: machine settings failed — "
    );
    assert_eq!(
        failure.strip_prefix(MACHINE_APPLY_FAILED_PREFIX),
        Some(
            "Universal Control could not be disabled for this host — the preference write did not land (`aterm pkg doctor` reads the keys)"
        )
    );
    assert_eq!(
        entries.as_slice(),
        std::slice::from_ref(&spotlight),
        "the successful action survives"
    );
    assert_eq!(
        machine_settings_line(&entries),
        "machine-settings: spotlight-noindex 2 dir(s) migrated",
        "a failed Universal Control write cannot mint its success event"
    );
    for outcome in [
        UniversalControlOutcome::NotApplicable,
        UniversalControlOutcome::AlreadyDisabled,
    ] {
        assert!(record_universal_control_outcome(outcome, &mut entries).is_none());
        assert_eq!(entries.as_slice(), std::slice::from_ref(&spotlight));
    }
    // The genuine success arm remains distinct and carries its revert.
    let success =
        record_universal_control_outcome(UniversalControlOutcome::Disabled, &mut entries).unwrap();
    assert!(!success.starts_with(MACHINE_APPLY_FAILED_PREFIX));
    assert!(success.contains(crate::machine::UNIVERSAL_CONTROL_REVERT));
    assert_eq!(
        entries,
        [
            spotlight,
            crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()
        ]
    );
}

/// `machine-settings:` is a wire contract the window parses: the marker and the
/// line it heads are byte-exact.
#[test]
fn the_machine_settings_marker_is_byte_stable() {
    assert_eq!(MACHINE_SETTINGS_MARKER, "machine-settings: ");
    assert_eq!(spotlight_entry(73), "spotlight-noindex 73 dir(s) migrated");
    assert_eq!(
        machine_settings_line(&[
            spotlight_entry(73),
            crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()
        ]),
        "machine-settings: spotlight-noindex 73 dir(s) migrated; universal-control disabled"
    );
    assert_eq!(
        machine_settings_line(&[crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()]),
        "machine-settings: universal-control disabled"
    );
    // The automatic pass's log filter: what it changed, plus every skip the user has
    // to act on — never the routine not-a-repo / live-build / not-applicable skips
    // or already-excluded rows. A crate whose cargo config cannot be pointed is
    // skipped (nothing renamed) on EVERY pass; before 2026-09-12 that was silent
    // there forever (review of audit K11).
    {
        use crate::noindex::Applied;
        let skip = |reason: &str| Applied::Skipped {
            path: PathBuf::from("/h/p/target"),
            reason: reason.to_string(),
        };
        let printed: Vec<String> = render_applied(&[
            skip(
                "/h/p/.cargo/config.toml could not be edited into valid TOML — point \
                 cargo at /h/p/target.noindex yourself — nothing was renamed",
            ),
            skip(
                "/h/p/target.noindex already exists and is never merged into — remove or \
                 rename it, then re-run",
            ),
            skip(
                "not beside a Cargo.toml — its pointer is an env var this cannot \
                 re-point; `aterm pkg noindex apply <dir>` migrates it by name",
            ),
            skip("a build holds /h/p/target/debug/.cargo-lock — retried next pass"),
            skip("not applicable"),
            skip("not applicable — Spotlight is a macOS index"),
            Applied::AlreadyExcluded(PathBuf::from("/h/q/target.noindex")),
        ])
        .into_iter()
        .filter(|l| noindex_pass_prints(l))
        .collect();
        assert_eq!(printed.len(), 2, "{printed:#?}");
        assert!(printed[0].starts_with("skipped /h/p/target — "));
        assert!(printed[0].contains("could not be edited into valid TOML"));
        assert!(printed[1].contains("already exists and is never merged into"));
    }
    // The apply renderer over one of each outcome. Its sentences are prose; what a
    // reader acts on is pinned: every outcome names its own path, a git checkout keeps
    // its symlink instead of a config edit, and the summary counts each kind.
    use crate::noindex::{Applied, ConfigNote, ExcludeNote, Pointer};
    let lines = render_applied(&[
        Applied::Migrated {
            from: PathBuf::from("/r/target"),
            to: PathBuf::from("/r/target.noindex"),
            config: ConfigNote::Written(PathBuf::from("/r/.cargo/config.toml")),
            discarded: false,
        },
        Applied::Migrated {
            from: PathBuf::from("/g/target"),
            to: PathBuf::from("/g/target.noindex"),
            config: ConfigNote::Linked {
                link: PathBuf::from("/g/target"),
                exclude: ExcludeNote::Added(PathBuf::from("/g/.git/info/exclude")),
                link_exclude: ExcludeNote::AlreadyIgnored,
            },
            discarded: false,
        },
        Applied::Migrated {
            from: PathBuf::from("/h/target"),
            to: PathBuf::from("/h/target.noindex"),
            config: ConfigNote::Linked {
                link: PathBuf::from("/h/target"),
                exclude: ExcludeNote::AlreadyIgnored,
                link_exclude: ExcludeNote::NotIgnored(
                    "the link shows as untracked, as the directory did".into(),
                ),
            },
            discarded: false,
        },
        Applied::Migrated {
            from: PathBuf::from("/d/target"),
            to: PathBuf::from("/d/target.noindex"),
            config: ConfigNote::Linked {
                link: PathBuf::from("/d/target"),
                exclude: ExcludeNote::Added(PathBuf::from("/d/.git/info/exclude")),
                link_exclude: ExcludeNote::Added(PathBuf::from("/d/.git/info/exclude")),
            },
            discarded: false,
        },
        Applied::AlreadyExcluded(PathBuf::from("/s/target.noindex")),
        Applied::Skipped {
            path: PathBuf::from("/t/target"),
            reason: "a build holds /t/target/debug/.cargo-lock — retried next pass".into(),
        },
    ]);
    for from in [
        "/r/target",
        "/g/target",
        "/h/target",
        "/d/target",
        "/t/target",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(from)),
            "{from} is named: {lines:#?}"
        );
    }
    assert!(
        lines.iter().any(|l| l.contains("/s/target.noindex")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("migrated /g/target") && l.contains("symlink /g/target")),
        "{lines:#?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("4 migrated, 1 already excluded, 1 skipped")
    );
    // Dry run: the summary says so, whatever the outcomes were.
    let lines = render_applied(&[
        Applied::Planned {
            from: PathBuf::from("/g/target"),
            to: PathBuf::from("/g/target.noindex"),
            pointer: Pointer::Symlink,
            discards: false,
        },
        Applied::Planned {
            from: PathBuf::from("/r/target"),
            to: PathBuf::from("/r/target.noindex"),
            pointer: Pointer::Config(PathBuf::from("/r/.cargo/config.toml")),
            discards: false,
        },
        Applied::Planned {
            from: PathBuf::from("/f/free"),
            to: PathBuf::from("/f/free.noindex"),
            pointer: Pointer::None,
            discards: false,
        },
    ]);
    assert!(lines.iter().any(|l| l.contains("/f/free")), "{lines:#?}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("3 would migrate, 0 already excluded, 0 skipped (dry run)")
    );
}

/// The stdout markers are a cross-crate contract (`crates/aterm-gui` parses them
/// byte-for-byte): pin the two added 2026-09-10 beside the nine before them, and the
/// `managed-current:` payload shape the GUI splits on `; `.
#[test]
fn the_shadowed_and_managed_current_markers_are_byte_stable() {
    assert_eq!(SHADOWED_MARKER, "shadowed: ");
    assert_eq!(MANAGED_CURRENT_MARKER, "managed-current: ");
    assert_eq!(LOCK_WAITING_MARKER, "lock-waiting: ");
    assert_eq!(LOCK_ACQUIRED_MARKER, "lock-acquired: ");
    assert_eq!(
        managed_current_line(&[]),
        None,
        "omitted when nothing qualifies"
    );
    assert_eq!(
        managed_current_line(&[
            ("claude".into(), "2.1.281".into(), "Anthropic".into()),
            ("codex".into(), "0.156.0".into(), "OpenAI".into()),
        ])
        .as_deref(),
        Some("managed-current: claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)")
    );
}

/// THE MANAGED-CURRENT LINE NAMES VENDOR PROGRAMS (design §1.7): an agent program whose
/// active vendor build's row says it is the vendor's latest, and whose twin is that
/// build, is listed by version and source — never a store id, never an index pin; a
/// kept row, a row for another build, a vendor this run could not reach, and a missing
/// twin each leave it out. Every exit of the vendor lane says it — the update pass
/// whatever its index lane did (stopped, or with member failures) and `update
/// <program>`, the path `claude update` takes.
#[cfg(unix)]
#[test]
fn the_managed_current_line_lists_vendor_programs_at_their_vendors_latest() {
    let layout = temp_layout("managed-current-vendor");
    let claude = crate::vendor_direct::Version::parse("2.1.281")
        .unwrap()
        .build_id();
    let build = layout.build_dir("claude", claude);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_tools(
        &layout,
        &build,
        &[crate::store::ToolName::new("claude").unwrap()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    let row = |state: String, installed: u64| crate::ProgramStatus {
        installed_build: Some(installed),
        state,
        tree_root: String::new(),
    };
    record_status(
        &layout,
        "claude",
        row(crate::state::vendor_managed("2.1.281", "Anthropic"), claude),
        "up to date".into(),
    );
    let now = 1_790_035_200;
    crate::vendor_direct::ProgramStamp::record_check(
        &layout,
        "claude",
        now,
        Some(crate::vendor_direct::Version::parse("2.1.281").unwrap()),
        None,
    )
    .unwrap();
    let none = std::collections::BTreeSet::new();
    assert_eq!(
        managed_current_line(&managed_current_entries(&layout, &none, now)).as_deref(),
        Some("managed-current: claude 2.1.281 (Anthropic latest)")
    );
    // Unreachable this run: the row is an older check's, so the line says nothing.
    let unreached = std::collections::BTreeSet::from([String::from("claude")]);
    assert_eq!(
        managed_current_entries(&layout, &unreached, now),
        Vec::new()
    );
    // Nor when the last check that reached the vendor is more than a day old.
    assert_eq!(
        managed_current_entries(&layout, &none, now + 24 * 60 * 60 + 1),
        Vec::new(),
        "a stale check"
    );
    for (state, installed) in [
        (
            crate::state::vendor_kept("2.1.281", "held by local pin"),
            claude,
        ),
        (
            crate::state::vendor_managed("2.1.280", "Anthropic"),
            claude - 1,
        ),
    ] {
        record_status(&layout, "claude", row(state.clone(), installed), "x".into());
        assert_eq!(
            managed_current_entries(&layout, &none, now),
            Vec::new(),
            "{state}"
        );
    }
    record_status(
        &layout,
        "claude",
        row(crate::state::vendor_managed("2.1.281", "Anthropic"), claude),
        "up to date".into(),
    );
    std::fs::remove_file(layout.agent_shim(&crate::store::ToolName::new("claude").unwrap()))
        .unwrap();
    assert_eq!(
        managed_current_entries(&layout, &none, now),
        Vec::new(),
        "no twin"
    );
    fn body_of(src: &'static str, signature: &str) -> &'static str {
        let start = src.find(signature).expect("the function");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .expect("its closing brace");
        &src[start..end]
    }
    let src = include_str!("cli.rs");
    let stopped = body_of(src, "fn update_pass_stopped(");
    assert!(stopped.contains("print_managed_current(layout, &vendor.unchecked, "));
    // `cmd_update_vendor` is a wrapper; the verb's body is `update_vendor`, which
    // both doors (a person's and the window's head watch) run.
    let vendor_verb = body_of(src, "fn update_vendor(");
    assert!(vendor_verb.contains("print_managed_current(layout, &pass.unchecked, "));
    let pass = body_of(src, "fn run_update_pass(");
    let said = pass
        .find("print_managed_current(layout, &vendor.unchecked, ")
        .expect("the pass says it");
    assert!(
        said < pass.find("if failures == 0 {").expect("the verdict"),
        "said before the verdict splits on member failures"
    );
}

/// THE ONE REMEDY (2026-09-16): where the hook file for the invoking shell exists
/// (the parent process's, `caller_shell`; `$SHELL` only when that is no shell) it is
/// sourced in place — `. ~/.aterm/shell.d/00-atpkg.zsh`, `source …fish`, the dot-source for
/// pwsh — wired rc or not, and NEVER `exec $SHELL`, which inside an aterm tab drops
/// the tab's shell integration (measured on the owner's machine: the zsh wrapper
/// consumes `ATERM_ORIGINAL_ZDOTDIR`, so a re-exec'd zsh loads no integration; the
/// in-place source moved `which claude` from `~/.local/bin/claude` to
/// `<prefix>/agents/claude` with `ATPKG_AGENTS` exported). Only a shell without the
/// hook file gets a PATH line, in its own dialect.
#[test]
fn the_remedy_sources_the_hook_in_place_and_never_execs_the_shell() {
    let layout = temp_layout("remedy");
    let home = crate::testdir::dir("atpkg-remedy-home-");
    let shell_d = home.join(".aterm").join("shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    let agents = layout.agents_dir().display().to_string();
    // No hook written yet (nor an rc that could be wired): the PATH line, per dialect.
    assert_eq!(
        shell_remedy_for(Some("zsh"), Some(&home), &layout),
        format!("export PATH=\"{agents}:$PATH\"")
    );
    assert_eq!(
        shell_remedy_for(Some("fish"), Some(&home), &layout),
        format!("set -gx PATH \"{agents}\" $PATH")
    );
    assert_eq!(
        shell_remedy_for(Some("pwsh"), Some(&home), &layout),
        format!(
            "$env:PATH = \"{agents}{}$env:PATH\"",
            if cfg!(windows) { ";" } else { ":" }
        )
    );
    assert_eq!(
        shell_remedy_for(None, Some(&home), &layout),
        format!("export PATH=\"{agents}:$PATH\"")
    );
    assert_eq!(
        shell_remedy_for(Some("zsh"), None, &layout),
        format!("export PATH=\"{agents}:$PATH\"")
    );
    // The hooks written (what every pass does): the source line, and the rc's wiring
    // makes no difference — an rc block is never written here, and the remedy is the
    // same one the block itself runs.
    crate::hooks::write_hooks(
        &shell_d,
        &layout.bin_dir(),
        &layout.agents_dir(),
        &layout.reroute_dir(),
    )
    .unwrap();
    assert_eq!(
        crate::hooks::rc_hook_wired(&home, "zsh"),
        Some((false, "00-atpkg.zsh"))
    );
    assert_eq!(
        shell_remedy_for(Some("zsh"), Some(&home), &layout),
        ". ~/.aterm/shell.d/00-atpkg.zsh"
    );
    assert_eq!(
        shell_remedy_for(Some("bash"), Some(&home), &layout),
        ". ~/.aterm/shell.d/00-atpkg.bash"
    );
    // `sh` has no hook it can source (the bash hook is not POSIX sh): the PATH line.
    assert_eq!(
        shell_remedy_for(Some("sh"), Some(&home), &layout),
        format!("export PATH=\"{agents}:$PATH\"")
    );
    assert_eq!(
        shell_remedy_for(Some("fish"), Some(&home), &layout),
        "source ~/.aterm/shell.d/00-atpkg.fish"
    );
    assert_eq!(
        shell_remedy_for(Some("pwsh"), Some(&home), &layout),
        ". ~/.aterm/shell.d/00-atpkg.ps1"
    );
    assert_eq!(
        shell_remedy_for(Some("powershell"), Some(&home), &layout),
        ". ~/.aterm/shell.d/00-atpkg.ps1"
    );
    // A wired rc changes nothing: still the source, never `exec $SHELL`.
    std::fs::write(
        home.join(".zshrc"),
        "# >>> atpkg shell integration >>>\n. ~/.aterm/shell.d/00-atpkg.zsh\n# <<< atpkg shell integration <<<\n",
    )
    .unwrap();
    assert_eq!(
        crate::hooks::rc_hook_wired(&home, "zsh"),
        Some((true, "00-atpkg.zsh"))
    );
    assert_eq!(
        shell_remedy_for(Some("zsh"), Some(&home), &layout),
        ". ~/.aterm/shell.d/00-atpkg.zsh"
    );
    // A shell atpkg has no hook for: the PATH line.
    assert_eq!(
        shell_remedy_for(Some("nu"), Some(&home), &layout),
        format!("export PATH=\"{agents}:$PATH\"")
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// `atpkg which` (design S6) and the SHADOW reconcile (design S5), over a real store:
/// a managed shim answers `managed <build> — pinned by index <N>` with the shim → store
/// resolution; a foreign copy AHEAD of the managed bin/ turns that into `SHADOWED by
/// <path>` (recorded by the pass, never fixed); a foreign copy with NO managed shim is
/// the `system:` state; a consent stub is the `extra` state; nothing is the fix line.
#[cfg(unix)]
#[test]
fn which_names_the_copy_that_runs_and_shadowing_is_read_per_path() {
    let layout = temp_layout("which");
    let build = layout.build_dir("trust", 6808);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/trust"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/trust"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &build,
        &["trust".to_string()],
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    record_status(
        &layout,
        "trust",
        crate::ProgramStatus {
            installed_build: Some(6808),
            state: crate::state::managed(6808, 41),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    let managed_only = std::env::join_paths([layout.bin_dir()]).unwrap();
    assert_eq!(
        which_line(&layout, "trust", Some(&managed_only)).unwrap(),
        format!(
            "trust → {} → {} — managed 6808 — pinned by index 41",
            layout
                .shim(&crate::store::ToolName::new("trust").unwrap())
                .display(),
            crate::which(&layout, "trust").unwrap().display()
        )
    );
    // A foreign copy AHEAD of the managed bin/.
    let local = layout
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-main-which-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();
    for name in ["trust", "gh"] {
        let exe = local.join(name);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ahead = std::env::join_paths([local.clone(), layout.bin_dir()]).unwrap();
    let shadowed = crate::state::shadowed(6808, &local.join("trust"));
    // trust is ALab's own, so its alias is laid: the answer carries the ONE trailing
    // fix-line after the canonical state (the p11-kit `trust` story, §17.11).
    assert_eq!(
        which_line(&layout, "trust", Some(&ahead)).unwrap(),
        format!(
            "trust → {} — {shadowed} — type alab-trust for the managed one",
            local.join("trust").display()
        )
    );
    assert_eq!(
        alias_fix(
            &layout,
            &crate::store::ToolName::new("trust").unwrap(),
            "trust",
            Some(&ahead)
        )
        .as_deref(),
        Some("type alab-trust for the managed one"),
        "the pass log composes the same sentence after the same state"
    );
    // A foreign `alab-trust` AHEAD of the managed bin/ too: the alias would not run
    // the managed copy either, so the fix-line is withheld (never a promise that does
    // not answer) and `which alab-trust` reports the alias as SHADOWED itself.
    {
        let foreign_alias = local.join("alab-trust");
        std::fs::write(&foreign_alias, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&foreign_alias, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            alias_fix(
                &layout,
                &crate::store::ToolName::new("trust").unwrap(),
                "trust",
                Some(&ahead)
            ),
            None,
            "no fix-line when the alias is shadowed too"
        );
        assert_eq!(
            which_line(&layout, "trust", Some(&ahead)).unwrap(),
            format!("trust → {} — {shadowed}", local.join("trust").display())
        );
        assert_eq!(
            which_line(&layout, "alab-trust", Some(&ahead)).unwrap(),
            format!(
                "alab-trust → {} — {}",
                foreign_alias.display(),
                crate::state::shadowed(6808, &foreign_alias)
            )
        );
        std::fs::remove_file(&foreign_alias).unwrap();
    }
    // The containment is ANCHORED: an `alab-<x>` that resolves into a `store/<x>/…`
    // tail OUTSIDE this prefix (a hand-made link into a checkout) earns no fix-line.
    {
        let checkout = local.join("store").join("trust").join("1").join("bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("trust"), b"#!/bin/sh\nexit 0\n").unwrap();
        let alias_shim = layout.shim(&crate::store::ToolName::new("alab-trust").unwrap());
        let managed_target = crate::platform::resolve_shim(&alias_shim).unwrap();
        std::fs::remove_file(&alias_shim).unwrap();
        std::os::unix::fs::symlink(checkout.join("trust"), &alias_shim).unwrap();
        assert_eq!(
            alias_fix(
                &layout,
                &crate::store::ToolName::new("trust").unwrap(),
                "trust",
                Some(&ahead)
            ),
            None,
            "a link outside the prefix is not the managed copy"
        );
        std::fs::remove_file(&alias_shim).unwrap();
        std::os::unix::fs::symlink(&managed_target, &alias_shim).unwrap();
    }
    // The alias itself resolves the managed copy, in the same canonical words as the
    // plain name — whatever PATH holds ahead of the managed bin/.
    let alias_shim = layout
        .shim(&crate::store::ToolName::new("alab-trust").unwrap())
        .display()
        .to_string();
    assert_eq!(
        which_line(&layout, "alab-trust", Some(&ahead)).unwrap(),
        format!(
            "alab-trust → {alias_shim} → {} — managed 6808 — pinned by index 41",
            crate::which(&layout, "trust").unwrap().display()
        )
    );
    // The pass SAYS it and records nothing (Phase 3: SHADOWED is one shell's truth);
    // a reader with that PATH computes it — the CANONICAL row, never the fix-line.
    let before = crate::status::read(&layout).unwrap().programs["trust"]
        .state
        .clone();
    assert_eq!(
        reconcile_shadowed(&layout, 41, Some(&ahead)),
        vec!["trust".to_string()]
    );
    let recorded = crate::status::read(&layout).unwrap();
    assert_eq!(recorded.programs["trust"].state, before, "never recorded");
    let seen = crate::status::with_shadows(&layout, recorded.clone(), Some(&ahead));
    assert_eq!(seen.programs["trust"].state, shadowed);
    assert!(
        !seen.programs["trust"].state.contains("alab-"),
        "the read state is exactly the canonical spelling"
    );
    assert_eq!(
        crate::status::with_shadows(&layout, recorded, Some(&managed_only)).programs["trust"].state,
        before,
        "a PATH with nothing ahead reads the record"
    );
    assert!(
        crate::which(&layout, "trust").is_some() && local.join("trust").exists(),
        "SHADOWED is said, never fixed: both copies are untouched"
    );
    // A row an older client recorded as SHADOWED is put back to the managed row.
    record_status(
        &layout,
        "trust",
        crate::ProgramStatus {
            installed_build: Some(6808),
            state: shadowed.clone(),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    assert!(reconcile_shadowed(&layout, 41, Some(&managed_only)).is_empty());
    assert_eq!(
        crate::status::read(&layout).unwrap().programs["trust"].state,
        crate::state::managed(6808, 41)
    );
    // Behind the managed bin/ the foreign copy does not run: no shadow.
    let behind = std::env::join_paths([layout.bin_dir(), local.clone()]).unwrap();
    assert!(reconcile_shadowed(&layout, 41, Some(&behind)).is_empty());
    // AN AGENT PROGRAM (2026-09-16). Its `agents/` twin is laid and current, so a
    // foreign copy ahead on THIS PATH is one shell's state: `which` names it as
    // such with the in-place remedy, the pass records the managed row (never
    // SHADOWED), names it on no `shadowed:` marker, and the out-ranked copies are
    // still listed. With `agents/` first, the managed answer as before.
    let v281 = vendor_version("2.1.281").build_id();
    let cbuild = layout.build_dir("claude", v281);
    std::fs::create_dir_all(cbuild.join("bin")).unwrap();
    std::fs::write(cbuild.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        cbuild.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &cbuild,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::store::mark_build_ready(&cbuild).unwrap();
    assert!(agent_twin_current(&layout, "claude", v281));
    assert!(
        !agent_twin_current(&layout, "claude", vendor_version("2.1.282").build_id()),
        "not that build"
    );
    assert!(
        !agent_twin_current(&layout, "trust", 6808),
        "no twin for an ALab tool"
    );
    let exe = local.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let no_agents = std::env::join_paths([local.clone(), layout.bin_dir()]).unwrap();
    let remedy = shell_remedy_command(&layout);
    let expect_absent =
        crate::state::agent_shadowed_in_shell(v281, &exe, &layout.agents_dir(), false, &remedy);
    // (An aterm shell's answer, where the remedy works; outside aterm the same PATH is
    // this shell's own copy, a note — since 2026-09-24.)
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let line = which_line_in(&layout, "claude", Some(&no_agents), inside).unwrap();
    assert_eq!(
        line,
        format!(
            "claude → {} — {expect_absent}\n{}",
            exe.display(),
            foreign_copies_line_with(FOREIGN_ON_THIS_SHELL, std::slice::from_ref(&exe))
        )
    );
    assert!(
        line.contains(&format!("`{remedy}` here picks the managed copy up")),
        "{line}"
    );
    assert!(
        remedy.contains("~/.aterm/shell.d/00-atpkg.")
            || remedy.starts_with("export PATH=\"")
            || remedy.starts_with("set -gx PATH ")
            || remedy.starts_with("$env:PATH = "),
        "the hook source, or a PATH line in the shell's dialect: {remedy}"
    );
    assert!(
        !remedy.starts_with("exec "),
        "never `exec $SHELL` — it drops the tab's shell integration (2026-09-16): {remedy}"
    );
    assert!(
        !line.contains("new tab") && !line.contains("remove or reorder"),
        "{line}"
    );
    assert!(
        !line.contains("out-ranked"),
        "the running copy is out-ranked by nothing here: {line}"
    );
    let agents_behind =
        std::env::join_paths([local.clone(), layout.bin_dir(), layout.agents_dir()]).unwrap();
    assert!(
        which_line_in(&layout, "claude", Some(&agents_behind), inside)
            .unwrap()
            .contains(&crate::state::agent_shadowed_in_shell(
                v281,
                &exe,
                &layout.agents_dir(),
                true,
                &remedy,
            )),
        "agents/ behind the foreign copy is named as such"
    );
    let agents_first =
        std::env::join_paths([layout.agents_dir(), local.clone(), layout.bin_dir()]).unwrap();
    let managed = which_line(&layout, "claude", Some(&agents_first)).unwrap();
    assert!(
        managed.starts_with(&format!(
            "claude → {} → ",
            layout
                .agent_shim(&crate::store::ToolName::new("claude").unwrap())
                .display()
        )) && managed.contains("managed 2.1.281"),
        "agents/ first: the managed copy runs, through the twin: {managed}"
    );
    // The pass: nothing shadowed, the managed row recorded, the tail printed.
    assert!(
        !reconcile_shadowed(&layout, 41, Some(&no_agents)).contains(&"claude".to_string()),
        "an agent program with a current twin is never on the shadowed marker"
    );
    assert!(
        !crate::status::read(&layout)
            .and_then(|s| s.programs.get("claude").cloned())
            .is_some_and(|r| crate::state::shadowed_by(&r.state).is_some()),
        "never recorded as SHADOWED"
    );
    // …and a row an OLDER client recorded as SHADOWED is put back to managed — for a
    // vendor program, its version and source, never an index pin.
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(v281),
            state: crate::state::shadowed(v281, &exe),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    assert!(!reconcile_shadowed(&layout, 41, Some(&no_agents)).contains(&"claude".to_string()));
    assert_eq!(
        crate::status::read(&layout).unwrap().programs["claude"].state,
        crate::state::vendor_source("2.1.281", "Anthropic")
    );
    // With the twin GONE (a lane that could not lay it), the plain SHADOWED row is
    // the truth again for that PATH: nothing in front of it vouches for the managed
    // copy — said by the pass, and read, never recorded.
    std::fs::remove_file(layout.agent_shim(&crate::store::ToolName::new("claude").unwrap()))
        .unwrap();
    assert!(!agent_twin_current(&layout, "claude", v281));
    assert!(
        reconcile_shadowed(&layout, 41, Some(&no_agents)).contains(&"claude".to_string()),
        "no twin: on the shadowed marker like any program"
    );
    let recorded = crate::status::read(&layout).unwrap();
    assert_eq!(
        recorded.programs["claude"].state,
        crate::state::vendor_source("2.1.281", "Anthropic")
    );
    assert_eq!(
        crate::status::with_shadows(&layout, recorded, Some(&no_agents)).programs["claude"].state,
        crate::state::shadowed(v281, &exe)
    );
    crate::activate::reconcile_agents(&layout);
    // A foreign copy with no managed shim: the system state, dated when a marker says so.
    assert_eq!(
        which_line(&layout, "gh", Some(&ahead)).unwrap(),
        format!(
            "gh → {} — {}",
            local.join("gh").display(),
            crate::state::system(&local.join("gh"), None)
        )
    );
    layout.record_retired("gh", "2026-08-27").unwrap();
    assert_eq!(layout.retired_date("gh").as_deref(), Some("2026-08-27"));
    assert!(
        which_line(&layout, "gh", Some(&ahead))
            .unwrap()
            .ends_with("(managed copy retired 2026-08-27)")
    );
    layout.clear_retired("gh");
    assert_eq!(layout.retired_date("gh"), None);
    // A pending stub: the next pass installs it.
    crate::stub::write_pending_stub(&layout, &crate::store::ToolName::new("emacs").unwrap())
        .unwrap();
    let line = which_line(&layout, "emacs", Some(&managed_only)).unwrap();
    assert!(line.starts_with("emacs → "), "{line}");
    assert!(line.contains("(pending stub)"), "{line}");
    assert!(line.contains("the next pass installs it"), "{line}");
    // A program that is BOTH a pending stub and shadowed by a foreign copy: PATH
    // order decides what runs. Foreign copy ahead of the managed bin/ ⇒ it runs and
    // atpkg does not manage it; behind it ⇒ the stub runs and waits.
    let exe = local.join("emacs");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        which_line(&layout, "emacs", Some(&ahead)).unwrap(),
        format!(
            "emacs → {} — {}",
            exe.display(),
            crate::state::system(&exe, None)
        )
    );
    let stub_first = std::env::join_paths([layout.bin_dir(), local.clone()]).unwrap();
    let line = which_line(&layout, "emacs", Some(&stub_first)).unwrap();
    assert!(line.contains("(pending stub)"), "{line}");
    // Installed, it is a managed member like any other: the foreign copy ahead reads
    // SHADOWED.
    let cbuild = layout.build_dir("emacs", 7);
    std::fs::create_dir_all(cbuild.join("bin")).unwrap();
    std::fs::write(cbuild.join("bin/emacs"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        cbuild.join("bin/emacs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &cbuild,
        &["emacs".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    assert_eq!(
        which_line(&layout, "emacs", Some(&ahead)).unwrap(),
        format!(
            "emacs → {} — {}",
            exe.display(),
            crate::state::shadowed(7, &exe)
        )
    );
    let _ = std::fs::remove_file(&exe);
    // An AGENT PROGRAM's stub is a pending stub like any other: the pass installs it
    // unasked (owner decision 2026-09-10), and the line says so (audit 2026-09-14).
    crate::stub::write_pending_stub(&layout, &crate::store::ToolName::new("codex").unwrap())
        .unwrap();
    let line = which_line(&layout, "codex", Some(&managed_only)).unwrap();
    assert!(line.contains("(pending stub)"), "{line}");
    assert!(!line.contains("consent"), "{line}");
    assert!(line.contains("the next pass installs it"), "{line}");
    let _ = std::fs::remove_file(layout.shim(&crate::store::ToolName::new("codex").unwrap()));
    // A recorded non-shim state reads back verbatim.
    let unserved = crate::state::unavailable("x86_64-unknown-linux-gnu", "macOS only");
    record_status(
        &layout,
        "homebrew",
        crate::ProgramStatus {
            installed_build: None,
            state: unserved.clone(),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    assert_eq!(
        which_line(&layout, "homebrew", Some(&managed_only)).unwrap(),
        format!("homebrew → (nothing runs) — {unserved}")
    );
    // Nothing: the fix line, as an error.
    assert_eq!(
        which_line(&layout, "nothing", Some(&managed_only)).unwrap_err(),
        not_installed_fix("nothing")
    );
    // The REVERSE case, honestly: an `alab-<x>` nothing answers to says why there is
    // no alias instead of promising `aterm pkg install alab-x`. emacs is installed
    // above with the policy Off; with no cached index the line can
    // only say the tool is installed and not aliased. An unknown base gets the plain
    // name's fix line.
    let err = which_line(&layout, "alab-emacs", Some(&managed_only)).unwrap_err();
    assert!(err.starts_with("atpkg: no alias alab-emacs — "), "{err}");
    assert!(err.ends_with("type emacs"), "{err}");
    assert_eq!(
        which_line(&layout, "alab-nothing", Some(&managed_only)).unwrap_err(),
        format!(
            "atpkg: no alias alab-nothing — {}",
            not_installed_fix("nothing")
        )
    );
    // The cached-index arms, driven directly: a system-satisfiable member and an
    // agent program never alias; an ALab tool that is not installed gets its own fix
    // line.
    let index = index_of(&[("ay", None), ("gh", Some("gh")), ("codex", None)]);
    assert_eq!(
        no_alias_line_for(&layout, "alab-gh", "gh", index.program("gh")),
        "atpkg: no alias alab-gh — gh is not one of ALab's own tools (a system copy may \
         satisfy it: system = \"gh\"); type gh"
    );
    assert_eq!(
        no_alias_line_for(&layout, "alab-codex", "codex", index.program("codex")),
        "atpkg: no alias alab-codex — codex is not one of ALab's own tools (a vendor's); \
         type codex"
    );
    assert_eq!(
        no_alias_line_for(&layout, "alab-ay", "ay", index.program("ay")),
        format!(
            "atpkg: alab-ay names nothing yet — {}",
            not_installed_fix("ay")
        )
    );
    // A name the cached index does not serve: install's own refusal, never a promise
    // of an install that refuses. A served name, a vendor program, and no cached
    // index at all keep the fix line.
    assert_eq!(
        unserved_which_line("nosuch", Some(&index)).as_deref(),
        Some("atpkg: nosuch is not named in the signed index (it names: ay, codex, gh)")
    );
    assert_eq!(unserved_which_line("ay", Some(&index)), None);
    assert_eq!(unserved_which_line("claude", Some(&index)), None);
    assert_eq!(unserved_which_line("nosuch", None), None);
    assert!(which_line(&layout, "alab-sudo", Some(&managed_only)).is_err());
    assert!(which_line(&layout, "../x", Some(&managed_only)).is_err());
    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `aterm pkg which` for a REROUTED upstream name (design S6, philosophy §4): the
/// answer is the row — the branded command(s) with `<args>`, the escape where an
/// upstream copy on PATH makes it reach something — and the stub's state, for a name
/// the deny-list refuses (`cargo`) and one it admits (`rustfmt`) alike; a foreign
/// occupant is named, never claimed; every other name keeps its behaviour (`rustup` is
/// not a row and stays refused as a tool name).
#[cfg(unix)]
#[test]
fn which_answers_a_rerouted_name_from_the_table() {
    let layout = temp_layout("which-reroute");
    let empty = std::ffi::OsString::new();
    // Upstream copies outside the prefix: the escape reaches them.
    let upstream = layout.prefix.with_extension("upstream");
    let _ = std::fs::remove_dir_all(&upstream);
    std::fs::create_dir_all(&upstream).unwrap();
    for name in ["cargo", "rustfmt"] {
        let exe = upstream.join(name);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let with_upstream = upstream.clone().into_os_string();
    // Before any stub is laid the row still answers, and says so.
    let line = which_line(&layout, "cargo", Some(&empty)).unwrap();
    assert!(line.contains("(reroute stub, missing)"), "{line}");
    crate::reroute::lay(&layout).unwrap();
    let cargo = which_line(&layout, "cargo", Some(&with_upstream)).unwrap();
    assert_eq!(cargo.lines().count(), 1, "{cargo}");
    assert!(
        cargo.starts_with(&format!(
            "cargo → {} (reroute stub, laid) — rerouted inside aterm sessions: announced, \
             naming ",
            crate::reroute::stub_path(&layout, "cargo").display()
        )),
        "{cargo}"
    );
    for needle in [
        "'targo trust <args>' / 'targo --unverified <args>'",
        "'cargo clippy' → 'targo tippy'",
        // The 2026-09-08 ruling, on the "which copy runs and why" surface:
        // a SIGNPOST row is not a refusal and must not read as one here.
        ", then run upstream ",
        "(`aterm --no-reroute` restores upstream 'cargo'.)",
    ] {
        assert!(!cargo.contains("refused"), "{cargo}");
        assert!(cargo.contains(needle), "{needle:?} missing from {cargo}");
    }
    let rustfmt = which_line(&layout, "rustfmt", Some(&with_upstream)).unwrap();
    assert!(rustfmt.contains("runs 'trustfmt <args>'"), "{rustfmt}");
    assert!(
        rustfmt.contains("`aterm --no-reroute` restores upstream 'rustfmt'"),
        "{rustfmt}"
    );
    // No upstream copy on PATH: an `aterm --no-reroute` session has none either, so
    // no escape is promised — as the stub's own line withholds it.
    for name in ["rustfmt", "clippy", "cargo"] {
        let line = which_line(&layout, name, Some(&empty)).unwrap();
        assert!(!line.contains("aterm --no-reroute"), "{line}");
        assert!(!line.contains("then run upstream"), "{line}");
    }
    // A SIGNPOST row with no upstream copy says so, as the stub's own line does —
    // and no more: `cargo trust <args>` / `cargo --unverified <args>` still run targo.
    assert!(
        which_line(&layout, "cargo", Some(&empty))
            .unwrap()
            .ends_with("; no upstream 'cargo' is on PATH"),
        "no upstream"
    );
    assert!(
        which_line(&layout, "rustfmt", Some(&empty))
            .unwrap()
            .ends_with("runs 'trustfmt <args>' with one stderr line"),
        "the clause ends where the escape would have been"
    );
    // A foreign occupant of a row's name: named as such, never claimed as ours.
    std::fs::write(
        crate::reroute::stub_path(&layout, "z3"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    let z3 = which_line(&layout, "z3", Some(&empty)).unwrap();
    assert!(z3.contains("(reroute stub, foreign)"), "{z3}");
    // Either mode names ay's relation to z3 (`[reroute] strict` is this machine's),
    // and either says no upstream copy is there to reach.
    assert!(
        z3.contains("'ay'")
            && z3.contains("measured against")
            && z3.ends_with("; no upstream 'z3' is on PATH"),
        "{z3}"
    );
    // Not a row: the deny-list still answers.
    assert_eq!(
        which_line(&layout, "rustup", Some(&empty)).unwrap_err(),
        "atpkg: \"rustup\" is not a tool name"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&upstream);
}

/// A dev-linked program whose own name is not a tool: `which trust` names the checkout
/// and the tools it exposes — and still names p11-kit's `trust` as unrelated.
#[test]
fn which_names_a_dev_linked_program_by_its_checkout_and_exposes() {
    let bins = vec!["trustc".to_string(), "targo".to_string()];
    let line = dev_linked_program_line(
        "trust",
        Some(std::path::Path::new("/h/trust/build/host/stage2")),
        &bins,
        Some(std::path::Path::new("/usr/bin/trust")),
    );
    assert!(line.starts_with("trust → /h/trust/build/host/stage2 (dev-linked; exposes targo, trustc — run `aterm pkg which targo`)"), "{line}");
    assert!(
        line.contains("`aterm pkg unlink trust` returns it to the index"),
        "{line}"
    );
    assert!(
        line.contains("/usr/bin/trust on PATH is an unrelated binary"),
        "{line}"
    );
    assert!(
        !line.contains("not installed"),
        "a linked program is not 'not installed': {line}"
    );
    let bare = dev_linked_program_line("trust", None, &[], None);
    assert!(
        bare.contains("exposes no command of its own") && !bare.contains('\n'),
        "{bare}"
    );
}

/// The p11-kit collision (2026-09-14, Linux): `which trust` answered
/// `trust → /usr/bin/trust — system: /usr/bin/trust — not managed by aterm`,
/// presenting a certificate-store tool as the Trust compiler, with no word that the
/// program is unserved on this target. The truth leads, the foreign binary is named
/// for what it is, and the phrase that misled — "system" as in "the system copy of
/// this program" — is gone.
#[test]
fn an_uninstalled_index_program_names_a_same_named_binary_as_unrelated() {
    let path = std::path::Path::new("/usr/bin/trust");
    let unserved = "unavailable on x86_64-unknown-linux-gnu: no build is published for this target";
    let line = index_program_shadowed_by_unrelated_line("trust", Some(unserved), path);
    assert!(
        line.starts_with("trust → (nothing runs) — unavailable on x86_64"),
        "{line}"
    );
    assert!(
        line.contains("/usr/bin/trust on PATH is an unrelated binary, not this program"),
        "{line}"
    );
    assert!(
        !line.contains("system:"),
        "the misleading 'system copy' phrasing must not survive: {line}"
    );

    // No recorded row: plain not-installed, with the fix — still never "system".
    let line = index_program_shadowed_by_unrelated_line("trust", None, path);
    assert!(
        line.contains("not installed (fix: aterm pkg install trust)"),
        "{line}"
    );
    assert!(line.contains("unrelated binary"), "{line}");
    assert!(!line.contains("system:"), "{line}");

    // A row that does not explain absence (e.g. a managed pin) gets the plain answer too.
    let line =
        index_program_shadowed_by_unrelated_line("trust", Some("managed 8595 — index 30"), path);
    assert!(line.contains("not installed"), "{line}");
}

/// THE ROLLED-BACK ROW CLAIMS NO PIN (audit, 2026-09-25): an index-program rollback
/// records `managed <to> — rolled back from <from>` — managed, never a `pinned by
/// index N` for a build index N does not pin — and `which`'s state reader prints those
/// words for that build instead of re-minting the pin.
#[test]
fn a_rolled_back_row_records_what_happened_and_which_prints_it() {
    let words = crate::state::rolled_back(7345, 8606);
    assert_eq!(words, "managed 7345 — rolled back from 8606");
    assert!(crate::state::is_managed(&words));
    assert_eq!(crate::state::managed_pin(&words), None, "no pin is claimed");
    assert!(
        crate::vendor_direct::retired_wording(&words).is_none(),
        "no retired index wording: {words}"
    );
    let layout = temp_layout("rolled-back-row");
    let row = crate::ProgramStatus {
        installed_build: Some(7345),
        state: words.clone(),
        tree_root: String::new(),
    };
    // No cached index in this fresh layout: the row's own words, verbatim.
    assert_eq!(managed_state_of(&layout, "clean", 7345, Some(&row)), words);
    // Another build's row is not this build's words.
    assert!(
        managed_state_of(&layout, "clean", 8606, Some(&row)).starts_with("managed 8606"),
        "{}",
        managed_state_of(&layout, "clean", 8606, Some(&row))
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE INDEX-PROGRAM ROLLBACK LINE, byte for byte (audit, 2026-09-25): what moved, what
/// the next update does, the one hold — and for a coherence-group member, that the
/// hold covers the whole group, in the same line, with no stderr warning offering a
/// second remedy.
#[test]
fn the_rollback_line_is_exact_and_names_the_group_it_holds() {
    assert_eq!(
        rollback_line("ay", 8256, 8250, None, &[], None),
        "atpkg: rolled back ay from build 8256 to 8250; the next update moves it forward \
         again \u{2014} `aterm pkg pin ay` holds it there"
    );
    assert_eq!(
        rollback_line("trust", 9192, 9178, Some("rustc"), &[], None),
        "atpkg: rolled back trust from build 9192 to 9178; the next update moves it \
         forward again \u{2014} `aterm pkg pin trust` holds it, and with it the whole rustc \
         group"
    );
    // Already pinned — its own pin, or a group sibling's — no update moves it: the line
    // says so and names the unpin, never a pin that is already there.
    let held = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        rollback_line("ay", 8256, 8250, None, &held(&["ay"]), None),
        "atpkg: rolled back ay from build 8256 to 8250; it is pinned, so updates leave it \
         there until `aterm pkg unpin ay`"
    );
    assert_eq!(
        rollback_line("trust", 9192, 9178, Some("rustc"), &held(&["ay"]), None),
        "atpkg: rolled back trust from build 9192 to 9178; ay is pinned, so updates leave \
         the whole rustc group there until `aterm pkg unpin ay`"
    );
    assert_eq!(
        rollback_line(
            "trust",
            9192,
            9178,
            Some("rustc"),
            &held(&["ay", "trust"]),
            None
        ),
        "atpkg: rolled back trust from build 9192 to 9178; ay and trust are pinned, so \
         updates leave the whole rustc group there until `aterm pkg unpin ay` and `aterm \
         pkg unpin trust`"
    );
    // A sibling's revoked current build: the next update moves the group whatever is
    // pinned, so the line names that build and offers no pin.
    assert_eq!(
        rollback_line("trust", 9192, 9178, Some("rustc"), &[], Some(("ay", 8250))),
        "atpkg: rolled back trust from build 9192 to 9178; the next update moves it \
         forward again (ay build 8250 is revoked)"
    );
}

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// An install unit for the set-refusal tests: `(member, tree, asset)` per member.
fn unit(group: Option<&str>, members: &[(&str, u64, u64)]) -> SetUnit {
    SetUnit {
        group: group.map(str::to_string),
        members: members.iter().map(|(m, ..)| (*m).to_string()).collect(),
        bytes: members.iter().fold(
            crate::flow::LandingBytes::default(),
            |acc, &(_, tree, asset)| acc.and(crate::flow::LandingBytes { asset, tree }),
        ),
    }
}

/// The `not enough disk space` sentence a unit's own gate writes for `need` (reserve
/// excluded) on `have` free.
fn gate_sentence(need: u64, have: u64) -> String {
    crate::FlowError::InsufficientDisk {
        required: need + crate::cost::FREE_FLOOR,
        available: have,
    }
    .to_string()
}

/// The set's need is the peak the per-unit gates enforce, archives included: freeing
/// exactly the shortfall the line names lets the tuple's own gate pass. The rustc tuple
/// alone (trust 3 GiB + a 1 GiB archive, three 100 MiB siblings with 50 MiB archives) on
/// 4 GiB free needs 5.4 GiB; a line summing installed sizes alone said 4.3 GiB, and a
/// volume freed to that still failed the group gate at 5.4 GiB.
#[test]
fn default_set_disk_refusal_names_the_peak_its_gates_enforce() {
    let prefix = std::path::Path::new("/Users//x/.aterm");
    let rustc = unit(
        Some("rustc"),
        &[
            ("trust", 3 * GIB, GIB),
            ("trust-cg", 100 * MIB, 50 * MIB),
            ("trust-ir", 100 * MIB, 50 * MIB),
            ("trust-vc", 100 * MIB, 50 * MIB),
        ],
    );
    let units = [rustc.clone()];
    let r = set_disk_refusal(&units, 4 * GIB, prefix).expect("refuses");
    // The figures are the contract: the peak need, what is free, the
    // shortfall, the volume — and no `install` offered for a tuple member.
    for figure in ["5.4 GiB", "4.0 GiB", "1.4 GiB", "/Users//x/.aterm"] {
        assert!(r.line.contains(figure), "{figure}: {}", r.line);
    }
    assert!(!r.line.contains("aterm pkg install"), "{}", r.line);
    // The tuple cannot fit by itself: every member carries the tuple's own figure.
    let tuple = gate_sentence(rustc.bytes.need(), 4 * GIB);
    assert_eq!(
        r.rows,
        ["trust", "trust-cg", "trust-ir", "trust-vc"]
            .map(|m| (m.to_string(), tuple.clone()))
            .to_vec()
    );
    // Freed to exactly the need: the set passes, and so does the gate it stands for.
    let need = crate::flow::peak_need([rustc.bytes]) + crate::cost::FREE_FLOOR;
    assert!(set_disk_refusal(&units, need, prefix).is_none());
    assert!(crate::cost::disk_ok(
        rustc.bytes.need(),
        need,
        crate::cost::FREE_FLOOR
    ));
    assert!(set_disk_refusal(&units, need - 1, prefix).is_some());
}

/// The offer is a unit whose own gate passes: an ungrouped program whose archive and
/// tree fit beside the reserve — never one whose tree alone fits (ay: 2.5 GiB + a 1 GiB
/// archive on 4 GiB free fails its gate), and never a tuple member, which `aterm pkg
/// install` lands outside its tuple, however small.
#[test]
fn default_set_disk_refusal_offers_only_an_ungrouped_program_its_gate_admits() {
    let prefix = std::path::Path::new("/Users//x/.aterm");
    let ay = unit(None, &[("ay", 5 * GIB / 2, GIB)]);
    let trust = unit(None, &[("trust", 4 * GIB, GIB)]);
    let r = set_disk_refusal(&[ay.clone(), trust], 4 * GIB, prefix).expect("refuses");
    assert!(!r.line.contains("aterm pkg install"), "{}", r.line);
    assert_eq!(
        r.rows[0],
        ("ay".to_string(), gate_sentence(ay.bytes.need(), 4 * GIB))
    );

    let rustc = unit(
        Some("rustc"),
        &[("trust", 3 * GIB, GIB), ("trust-cg", 100 * MIB, 50 * MIB)],
    );
    let ty = unit(None, &[("ty", 300 * MIB, 100 * MIB)]);
    let r = set_disk_refusal(&[rustc.clone(), ty], 4 * GIB, prefix).expect("refuses");
    assert!(
        r.line
            .ends_with(", or install ty alone: aterm pkg install ty"),
        "{}",
        r.line
    );
    let tuple = gate_sentence(rustc.bytes.need(), 4 * GIB);
    let set = r.line.strip_prefix("atpkg: ").unwrap();
    let set = set.split(", or install").next().unwrap().to_string();
    assert_eq!(
        r.rows,
        vec![
            ("trust".to_string(), tuple.clone()),
            ("trust-cg".to_string(), tuple),
            ("ty".to_string(), set),
        ],
        "the tuple's members carry its figure, the program that fits the set's"
    );

    // A tuple that fits by itself is still no offer: its members install together.
    let small = unit(
        Some("rustc"),
        &[
            ("trust-cg", 100 * MIB, 50 * MIB),
            ("trust-vc", 100 * MIB, 50 * MIB),
        ],
    );
    let trust = unit(None, &[("trust", 4 * GIB, GIB)]);
    let r = set_disk_refusal(&[small, trust.clone()], 4 * GIB, prefix).expect("refuses");
    assert!(!r.line.contains("aterm pkg install"), "{}", r.line);
    assert_eq!(
        r.rows[2],
        (
            "trust".to_string(),
            gate_sentence(trust.bytes.need(), 4 * GIB)
        )
    );
}

/// Every member the refused set would add gets a CURRENT row, so a stale figure from an
/// earlier pass cannot outlive it: three programs that each fit by themselves, refused
/// on their sum, all record the set's sentence — and a member held by its memo keeps its
/// held row.
#[test]
fn a_refused_set_rewrites_every_member_row_and_spares_a_held_one() {
    use crate::flow::{LandingBytes, PlannedLanding};
    let layout = temp_layout("set-disk-rows");
    let prefix = std::path::Path::new("/Users//x/.aterm");
    let held = crate::state::held_failed("tree_root mismatch", "now: aterm pkg install gh");
    record_unbuilt(&layout, "gh", held.clone(), String::new());
    record_bootstrap_failure(&layout, "trust", &gate_sentence(4 * GIB, 4 * GIB));
    let groups = ["ay", "gh", "trust", "ty"].map(|m| crate::Group {
        group: None,
        members: vec![m.to_string()],
    });
    let planned: Vec<_> = groups.iter().map(|g| (g, g.members.clone())).collect();
    let units = set_units(&planned, |g| match g.members[0].as_str() {
        "gh" => PlannedLanding::Nothing,
        "trust" => PlannedLanding::Bytes(LandingBytes {
            asset: MIB,
            tree: 3 * GIB,
        }),
        _ => PlannedLanding::Bytes(LandingBytes {
            asset: MIB,
            tree: 2 * GIB,
        }),
    })
    .expect("every landing unit is known");
    let r = set_disk_refusal(&units, 5 * GIB, prefix).expect("7 GiB on 5 GiB refuses");
    assert!(
        r.line
            .ends_with("free 3.0 GiB, or install ay alone: aterm pkg install ay"),
        "{}",
        r.line
    );
    r.record(&layout);
    let status = crate::status::read(&layout).expect("rows recorded");
    let set = r.line.strip_prefix("atpkg: ").unwrap();
    let set = set.split(", or install").next().unwrap();
    for m in ["ay", "trust", "ty"] {
        assert_eq!(status.programs[m].state, format!("error: {set}"), "{m}");
    }
    assert_eq!(
        status.programs["gh"].state, held,
        "a held row is never overwritten"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The units the set check judges: a unit its lane lands nothing of is left out — on
/// 4.5 GiB free, a 4 GiB tuple that a sibling's local pin holds no longer refuses a
/// 300 MiB program, nor does a held one — while any other unknown leaves the check
/// unanswered. A tuple's unit carries the members the pass adds and the bytes its own
/// gate sums, which can count more than those members.
#[test]
fn set_units_leave_out_the_units_the_pass_lands_nothing_of() {
    use crate::flow::{LandingBytes, PlannedLanding};
    let bytes = |tree| PlannedLanding::Bytes(LandingBytes { asset: MIB, tree });
    let ungrouped = |m: &str| crate::Group {
        group: None,
        members: vec![m.to_string()],
    };
    let rustc = crate::Group {
        group: Some("rustc".to_string()),
        members: ["trust", "trust-cg", "trust-ir"].map(String::from).to_vec(),
    };
    let (ay, ty) = (ungrouped("ay"), ungrouped("ty"));
    let planned = [
        (&ay, ay.members.clone()),
        (&ty, ty.members.clone()),
        (&rustc, vec!["trust".to_string()]),
    ];
    let prefix = std::path::Path::new("/Users//x/.aterm");
    let have = 9 * GIB / 2;
    let lands = |tuple: PlannedLanding| {
        move |g: &crate::Group| match g.members[0].as_str() {
            "ay" => PlannedLanding::Nothing,
            "ty" => bytes(300 * MIB),
            _ => tuple,
        }
    };
    let units = set_units(&planned, lands(PlannedLanding::Nothing)).expect("all known");
    assert_eq!(
        units,
        vec![SetUnit {
            group: None,
            members: vec!["ty".to_string()],
            bytes: LandingBytes {
                asset: MIB,
                tree: 300 * MIB
            },
        }]
    );
    assert!(set_disk_refusal(&units, have, prefix).is_none());
    // The same tuple counted: the set refuses — what leaving it out spares.
    let units = set_units(&planned, lands(bytes(4 * GIB))).expect("all known");
    assert!(set_disk_refusal(&units, have, prefix).is_some());
    assert_eq!(
        units[1],
        SetUnit {
            group: Some("rustc".to_string()),
            members: vec!["trust".to_string()],
            bytes: LandingBytes {
                asset: MIB,
                tree: 4 * GIB
            },
        }
    );
    assert!(set_units(&planned, lands(PlannedLanding::Unknown)).is_none());
}

/// A pass whose only work was a leftover download in `staging/` freed disk: `gc` must
/// not say "nothing to reclaim" over the line naming what it removed.
#[test]
fn gc_staging_sweep_alone_is_not_nothing_to_reclaim() {
    let mut report = crate::gc::GcReport::default();
    assert!(gc_freed_nothing(&report));
    report.swept_staging = vec![("ay".to_string(), vec!["ay-17.tar.zst".to_string()])];
    assert!(!gc_freed_nothing(&report));
    assert_eq!(
        gc_sweep_lines("gc", &report),
        vec!["atpkg gc: removed leftover download(s) of ay: ay-17.tar.zst".to_string()]
    );
}

/// THE PIN, UNPIN AND DEV-LINKED LINES, byte for byte (audit, 2026-09-25): the build
/// named in `list`'s spelling, the state, one thing to know — the pin's one exception
/// as a fact, the unpin naming the check instead of predicting it, the dev-linked skip
/// naming the program's own source (the vendor for claude/codex, the registry for the
/// rest).
#[test]
fn pin_unpin_and_dev_linked_lines_are_exact() {
    let v = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    assert_eq!(
        pin_line("ay", 8256, true, true, PinPeers::Alone),
        "atpkg: ay build 8256 pinned \u{2014} updates leave it there until `aterm pkg unpin \
         ay` (a yanked build is still replaced)"
    );
    assert_eq!(
        pin_line("claude", v, true, true, PinPeers::Alone),
        "atpkg: claude 2.1.283 pinned \u{2014} updates leave it there until `aterm pkg \
         unpin claude` (a yanked build is still replaced)"
    );
    assert_eq!(
        pin_line("claude", v, false, true, PinPeers::Alone),
        "atpkg: claude 2.1.283 unpinned \u{2014} updates resume; `aterm pkg update claude` \
         checks now"
    );
    assert_eq!(
        pin_line("ay", 8256, false, true, PinPeers::Alone),
        "atpkg: ay build 8256 unpinned \u{2014} updates resume; `aterm pkg update ay` \
         checks now"
    );
    assert_eq!(
        pin_line("ay", 8256, true, false, PinPeers::Alone),
        "atpkg: ay build 8256 is already pinned (aterm pkg unpin ay releases it)"
    );
    assert_eq!(
        pin_line("ay", 8256, false, false, PinPeers::Alone),
        "atpkg: ay build 8256 was not pinned \u{2014} updates already move it"
    );
    assert_eq!(
        dev_linked_line("claude"),
        "atpkg: claude is dev-linked; run `aterm pkg unlink claude` to install Anthropic's \
         latest"
    );
    assert_eq!(
        dev_linked_line("codex"),
        "atpkg: codex is dev-linked; run `aterm pkg unlink codex` to install OpenAI's latest"
    );
    assert_eq!(
        dev_linked_line("ay"),
        "atpkg: ay is dev-linked; run `aterm pkg unlink ay` to install ALab's build"
    );
    for line in [
        pin_line("claude", v, true, true, PinPeers::Alone),
        pin_line("claude", v, false, true, PinPeers::Alone),
        dev_linked_line("claude"),
    ] {
        assert!(
            crate::vendor_direct::retired_wording(&line).is_none(),
            "no retired index wording, no store id: {line}"
        );
    }
}

/// A coherence-group member is held by ANY member's pin: the update pass's group gate
/// freezes the whole tuple ([`crate::flow::pin_gate_members`]). So the line names every
/// unpin the hold takes — the program's own when it is pinned, and each pinned
/// sibling's — and claims nothing about updates moving it while a sibling's pin holds
/// the tuple; with no cached index to read the group from, an unchanged pin predicts
/// nothing about the next update (review 27).
#[test]
fn a_group_member_s_pin_line_names_every_unpin_the_hold_takes() {
    // The programs a line's `aterm pkg unpin <p>` commands name, sorted.
    fn unpins(line: &str) -> Vec<&str> {
        let mut named: Vec<&str> = line
            .split("aterm pkg unpin ")
            .skip(1)
            .filter_map(|rest| {
                rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    .next()
            })
            .collect();
        named.sort_unstable();
        named
    }
    let claims_updates_move =
        |line: &str| line.contains("updates resume") || line.contains("updates already move");
    let trust = vec!["trust".to_string()];
    let two = vec!["ay".to_string(), "trust".to_string()];
    let group = |pinned| PinPeers::Group {
        group: "rustc",
        pinned,
    };
    for (pinned, changed, peers, want) in [
        (false, false, group(&trust), vec!["trust"]),
        (false, true, group(&two), vec!["ay", "trust"]),
        (true, false, group(&trust), vec!["trust", "trust-cg"]),
        (true, true, group(&trust), vec!["trust", "trust-cg"]),
    ] {
        let line = pin_line("trust-cg", 9178, pinned, changed, peers);
        assert_eq!(unpins(&line), want, "{line}");
        assert!(
            !claims_updates_move(&line) && !line.contains("aterm pkg update"),
            "a sibling's pin still holds the tuple: {line}"
        );
    }
    // Alone, the same verbs do say updates move it: the claim the group arm withholds.
    for changed in [false, true] {
        let line = pin_line("trust-cg", 9178, false, changed, PinPeers::Alone);
        assert!(claims_updates_move(&line), "{line}");
    }
    for pinned in [false, true] {
        let line = pin_line("trust-cg", 9178, pinned, false, PinPeers::Unknown);
        assert!(!line.contains("update"), "no prediction: {line}");
        let own: &[&str] = if pinned { &["trust-cg"] } else { &[] };
        assert_eq!(unpins(&line), own, "{line}");
    }
}

/// THE SELF-UPDATE VERB (2026-09-19, `crate::selfupdate`), in-process, every ending
/// that is not the child: usage exit 2 (a `--help` operand too — non-mutating); not a
/// tool name exit 2; a program with no row forwards to the `bin/` shim verbatim with
/// nothing printed (a twin laid by a newer roster must still run the tool); the
/// prefix cross-check refuses with the exact line when the store atpkg resolves here
/// differs from the operand, and proceeds when it agrees or cannot resolve at all;
/// `install stable`, `install 2.1.200` and `update --foo` are declined with the exact
/// lines and the fake shim never runs — and there is no escape hatch: the first cut's
/// environment knob was removed the same day at the owner's direction, so a declined
/// shape is declined whatever the environment says, and the plan has no input that
/// could say otherwise; `update --help` prints the help note and forwards verbatim;
/// the disabled manager refuses with its line, exit 1, nothing run; the hidden verb
/// is in no roster and takes no lock — and the store lock file never comes into
/// being for any of them. Then THE 75 ENDING (§8), in-process against a lock THIS
/// test holds: `selfupdate_check` with a ONE-SECOND bound — the parameter that
/// replaced the wait-bound env knob — runs the REAL dev `atpkg` (the bin beside this
/// test executable, through a wrapper named `atpkg` that confines it to a temp HOME
/// and captures its pipes) as `update claude --wait-lock 1`; the child waits its
/// bound at the held lock and exits 75 with its lock sentence on stderr and its
/// `seed-busy:` marker on stdout, and the verb relays `CONTENDED_EXIT` after a
/// contended line that renders the bound as `1 s`. The forward endings are pinned on
/// the PLAN — the command the verb would `exec`, run here as a child so its argv is
/// observed on a log — because an `exec` would replace the test process; the
/// enablement is the plan's input (production reads `manager_enabled()`) and the
/// child's confinement rides the wrapper, because `std::env::set_var` is a data race
/// in a threaded test binary (the rule `install.rs` records).
#[cfg(unix)]
#[test]
fn cmd_selfupdate_refuses_forwards_and_never_takes_the_lock() {
    use crate::selfupdate::{self, Decline};
    let layout = temp_layout("selfupdate");
    let other = temp_layout("selfupdate-other");
    let claude = crate::store::ToolName::new("claude").unwrap();
    let ay = crate::store::ToolName::new("ay").unwrap();
    let row = selfupdate::row_for("claude").unwrap();
    let log = layout.prefix.join("fake.log");
    // Fake `bin/` shims that record their argv, one per line, and exit 0.
    std::fs::create_dir_all(layout.bin_dir()).unwrap();
    for tool in [&claude, &ay] {
        let shim = layout.shim(tool);
        let mut body = String::from("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '");
        body.push_str(&log.display().to_string());
        body.push_str("'\nexit 0\n");
        std::fs::write(&shim, body).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let logged = || {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        text
    };
    // Runs a planned forward as a child (the verb would `exec` it) and answers its
    // note and the argv the fake shim recorded.
    let forwarded = |plan: SelfUpdatePlan| -> (Option<String>, String) {
        match plan {
            SelfUpdatePlan::Forward { mut command, note } => {
                assert!(
                    command
                        .get_program()
                        .to_string_lossy()
                        .ends_with("/bin/claude")
                        || command.get_program().to_string_lossy().ends_with("/bin/ay"),
                    "the bin/ shim, never the twin: {command:?}"
                );
                let out = command.output().unwrap();
                assert!(out.status.success(), "{out:?}");
                (note, logged())
            }
            other => panic!("expected a forward, got {other:?}"),
        }
    };
    // 1. Usage and the tool-name gate, through the verb itself (nothing runs).
    assert_eq!(cmd_selfupdate(&[]), ExitCode::from(2));
    assert_eq!(cmd_selfupdate(&args(&["--help"])), ExitCode::from(2));
    assert_eq!(
        main_entry(vec!["__selfupdate".into(), "--help".into()]),
        ExitCode::from(2),
        "dispatched, and non-mutating"
    );
    assert_eq!(
        cmd_selfupdate(&args(&["sudo", "--", "update"])),
        ExitCode::from(2),
        "a sensitive name is not a tool name"
    );
    assert_eq!(
        cmd_selfupdate(&args(&["", "--", "update"])),
        ExitCode::from(2)
    );
    // 2. No row: forwards verbatim, silent.
    let (note, argv) = forwarded(selfupdate_plan(
        &layout,
        &ay,
        &args(&["update", "--flag"]),
        Some(&layout),
        true,
    ));
    assert_eq!(note, None);
    assert_eq!(argv, "update\n--flag\n");
    // 3. The prefix cross-check.
    match selfupdate_plan(&layout, &claude, &args(&["update"]), Some(&other), true) {
        SelfUpdatePlan::Refuse(line) => assert_eq!(
            line,
            selfupdate::prefix_mismatch_line(row, &layout.prefix, &other.prefix)
        ),
        other => panic!("{other:?}"),
    }
    for resolved in [Some(&*layout), None] {
        match selfupdate_plan(&layout, &claude, &args(&["update"]), resolved, true) {
            SelfUpdatePlan::Check { row: r } => {
                assert_eq!(r.program, "claude");
            }
            other => panic!("{resolved:?}: {other:?}"),
        }
    }
    // 4. Declined shapes: the exact line, and the fake never runs. No input of the
    // plan can turn a decline into a forward — the escape hatch is gone.
    for (typed, decline) in [
        (
            &["install", "stable"][..],
            Decline::NotAChannel {
                verb: "install",
                target: "stable",
            },
        ),
        (
            &["install", "2.1.200"],
            Decline::Version {
                verb: "install",
                target: "2.1.200",
            },
        ),
    ] {
        for enabled in [true, false] {
            match selfupdate_plan(&layout, &claude, &args(typed), Some(&layout), enabled) {
                SelfUpdatePlan::Refuse(line) => {
                    assert_eq!(
                        line,
                        selfupdate::declined_line(row, &decline),
                        "{typed:?} enabled={enabled}"
                    );
                }
                other => panic!("{typed:?} enabled={enabled}: {other:?}"),
            }
        }
    }
    let rest = args(&["--foo"]);
    match selfupdate_plan(
        &layout,
        &claude,
        &args(&["update", "--foo"]),
        Some(&layout),
        true,
    ) {
        SelfUpdatePlan::Refuse(line) => assert_eq!(
            line,
            selfupdate::declined_line(
                row,
                &Decline::Shape {
                    verb: "update",
                    rest: &rest
                }
            )
        ),
        other => panic!("{other:?}"),
    }
    assert!(!log.exists(), "a declined shape runs nothing");
    // 5. Help: the note, then the vendor's own help, verbatim — enabled or not.
    for enabled in [true, false] {
        let (note, argv) = forwarded(selfupdate_plan(
            &layout,
            &claude,
            &args(&["update", "--help"]),
            Some(&layout),
            enabled,
        ));
        assert_eq!(
            note.as_deref(),
            Some(selfupdate::help_line(row).as_str()),
            "enabled={enabled}"
        );
        assert_eq!(argv, "update\n--help\n", "enabled={enabled}");
    }
    // A pass-through shape: forwards silently.
    let (note, argv) = forwarded(selfupdate_plan(
        &layout,
        &claude,
        &args(&["--bare", "update"]),
        Some(&layout),
        true,
    ));
    assert_eq!(note, None);
    assert_eq!(argv, "--bare\nupdate\n");
    // 6. The manager disabled: its line, exit 1, nothing run (no active build in this
    // layout — the fake shim names no store target — so the line says so), for
    // every check shape.
    for typed in [
        &["update"][..],
        &["upgrade"],
        &["install"],
        &["install", "latest", "--force"],
    ] {
        match selfupdate_plan(&layout, &claude, &args(typed), Some(&layout), false) {
            SelfUpdatePlan::Disabled(line) => {
                assert_eq!(line, selfupdate::disabled_line(row, None), "{typed:?}");
            }
            other => panic!("{typed:?}: {other:?}"),
        }
    }
    assert!(!log.exists(), "nothing ran under a disabled manager");
    // 7. The verb is machinery, not vocabulary, and takes no lock of its own.
    assert!(!dispatch_roster().contains(&selfupdate::HIDDEN_VERB));
    assert!(!verb_mutates_store(selfupdate::HIDDEN_VERB));
    assert_eq!(verb_usage(selfupdate::HIDDEN_VERB), None);
    assert!(
        !layout.store_lock().exists() && !other.store_lock().exists(),
        "no ending above touches the store lock — only the child does"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&other.prefix);
    // 8. THE 75 ENDING, against a lock THIS test holds. The child is the real dev
    // `atpkg`, run through a wrapper named `atpkg` (so `child_argv` hands it the argv
    // verbatim) that records that argv, pins HOME to a temp directory — the default
    // prefix under that HOME is the layout whose lock is held here, and the registry
    // is a local directory, so no real store and no network — and captures the
    // child's two pipes to files this lane reads back. A one-second bound: the
    // parameter that replaced the env knob, and inside the 2 s announcement grace,
    // so the child prints no `lock-waiting:` line, only its `seed-busy:` terminal.
    // That silence is also the lane's proof that the child waited "no longer" than
    // its bound, read off the CHILD's clock: see the assertions below for why this
    // test's own clock may bound the wait from below only.
    let home = crate::testdir::dir("atpkg-main-selfupdate-held-");
    let held = crate::store::Layout {
        prefix: crate::store::default_prefix(&home),
    };
    assert!(
        held.prefix.starts_with(&home),
        "the default prefix sits under the temp HOME: {}",
        held.prefix.display()
    );
    let guard = crate::lock::try_lock_store(&held).expect("this test holds the store lock");
    let dev_atpkg = dev_atpkg_binary();
    let wrapper_dir = home.join("wrapper");
    std::fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper = wrapper_dir.join("atpkg");
    let child_argv = home.join("child.argv");
    let child_out = home.join("child.stdout");
    let child_err = home.join("child.stderr");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexport HOME='{}' \
             XDG_CONFIG_HOME='{}' ATPKG_REGISTRY='dir:{}'\n\
             exec '{}' \"$@\" >'{}' 2>'{}'\n",
            child_argv.display(),
            home.display(),
            home.join("config").display(),
            home.join("registry").display(),
            dev_atpkg.display(),
            child_out.display(),
            child_err.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let code = selfupdate_check(&held, row, &wrapper, 1);
    let elapsed = started.elapsed();
    drop(guard);
    let child_stdout = std::fs::read_to_string(&child_out).unwrap_or_default();
    let child_stderr = std::fs::read_to_string(&child_err).unwrap_or_default();
    let shown = format!("child stdout:\n{child_stdout}\nchild stderr:\n{child_stderr}");
    assert_eq!(
        code,
        ExitCode::from(crate::lock::CONTENDED_EXIT),
        "the child's 75 relayed: {shown}"
    );
    assert_eq!(
        std::fs::read_to_string(&child_argv).unwrap_or_default(),
        "update\nclaude\n--wait-lock\n1\n",
        "the standard update with the test's own bound"
    );
    // WHICH lock the child met, before anything about how long it took: the one this
    // test holds, under its own per-process temp HOME — never the real prefix and
    // never a path another process's copy of this suite can reach.
    let lock = held.store_lock().display().to_string();
    assert!(
        child_stderr.lines().any(|l| {
            l.starts_with("atpkg: another atpkg process holds the store lock at")
                && l.contains(&lock)
        }),
        "the child's own lock sentence names the held lock: {shown}"
    );
    assert!(
        child_stdout.lines().any(|l| l
            == format!(
                "atpkg: {SEED_BUSY_MARKER}another atpkg process holds the store lock \
                 \u{2014} that pass is doing this work and this one stood aside"
            )),
        "the wait lane's terminal marker, on stdout: {shown}"
    );
    // THE WAIT, on two clocks, each read only in the direction it can answer.
    //
    // "No longer" is read off the CHILD's clock, never this one. `elapsed` also spans
    // the spawn, the `exec` of a wrapper written moments ago and of a dev binary that
    // may have been linked moments ago, the child's startup and its exit — and on
    // macOS the first `exec` of a new file waits on the system's assessment of it,
    // which nothing in this repo bounds. Measured 2026-09-24 with other agents
    // building: 0.2–3.5 s for a new file's first `exec` against under 11 ms for its
    // second, and this lane up to 5.7 s end to end for its 1 s wait, every slow run
    // silent on `lock-waiting:`. A gate run met 26 s — `seed-busy:`, no
    // `lock-waiting:` — against the 20 s ceiling this lane used to put on `elapsed`,
    // and 21 s of delay put in front of the child's `exec` reproduces that failure
    // exactly: a wait that ended on time, failed for process creation. The child's
    // own word cannot be stretched that way. `lock_store_waiting` weighs the
    // announcement grace BEFORE its deadline on every contended poll, so a child
    // whose deadline were 2 s or more would print `lock-waiting:` at the very poll
    // that ends its wait (handed `--wait-lock 3` instead, it does, and this fails). A
    // `seed-busy:` terminal with no `lock-waiting:` line is the child saying, on its
    // own monotonic clock from its first try, that its wait ended inside the grace:
    // the 1 s it was handed, not the 1800 s `WAIT_LOCK_SECS` nor the ten-minute
    // typed wait.
    assert!(
        !child_stdout.contains(LOCK_WAITING_MARKER),
        "a 1 s wait ends inside the 2 s announcement grace — the child's own clock \
         says it waited no longer than its bound ({elapsed:?} end to end, spawn and \
         exec included): {shown}"
    );
    // "It waited its bound" is read off THIS clock, whose interval contains the
    // child's whole wait: a child that waited its second can never read under it,
    // however loaded the machine, so this direction cannot fail on load. It can be
    // vacuous under load (process creation alone may take a second); the wait itself
    // is pinned with no process in between by
    // `lock::tests::lock_store_waiting_times_out_with_contended`, and the bound it
    // is handed by the argv above.
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "the child waited its bound: {elapsed:?}\n{shown}"
    );
    assert!(
        selfupdate::contended_line(row, None, 1, false).contains("still running after 1 s"),
        "the contended line the verb printed renders the bound it passed"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The dev `atpkg` binary beside this test executable — `target/<profile>/atpkg`,
/// two levels up from `deps/` — which `targo --unverified test -p atpkg` builds for
/// the integration lanes before any test runs. LOUD when absent: a lane that drives
/// the real child against a held lock must not pass on a fake one.
#[cfg(unix)]
fn dev_atpkg_binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("this test executable");
    let candidate = exe
        .parent()
        .and_then(std::path::Path::parent)
        .map(|dir| dir.join("atpkg"))
        .expect("target/<profile>/deps/<test> has a profile directory");
    assert!(
        crate::stub::is_live_atpkg(&candidate),
        "no dev atpkg binary at {} — `targo --unverified test -p atpkg` builds it (the \
         integration lanes need it too); this lane drives the real child against a \
         lock it holds",
        candidate.display()
    );
    candidate
}

/// DESIGN S7 on the `which` surface: a managed shim that exports its policy's
/// `shim_env` answers with the canonical state and ONE trailing sentence — `Claude
/// Code's own updater is off here (DISABLE_AUTOUPDATER=1)` — never inside the state; a
/// foreign copy
/// ahead on PATH runs WITHOUT the env, so the SHADOWED answer carries no such line;
/// and a plain shim never earns it. AND (2026-09-19, `crate::selfupdate`) a rostered
/// agent program's line ends with `` `claude update` here runs `aterm pkg update
/// claude` `` whether or not the shim exports an env — codex, which exports none,
/// gets it too — while a shadowed row and a non-agent program get no such sentence.
/// "Here" is shell-local, so the sentence is withheld (review, 2026-09-19) in a shell
/// whose `PATH` has `bin/` and not `agents/` (that shell runs `bin/claude`, which
/// carried no block until 2026-10-02 — the first cut pinned the sentence in exactly
/// that shell), when
/// the twin is not laid at all, and when the laid twin is yesterday's landing-only
/// twin (it resolves where `bin/` does and still runs the vendor's updater until a
/// pass re-lays it). A vendor program (design §1.7) is named by version and source,
/// never by its store id or an index pin.
#[cfg(unix)]
#[test]
fn which_names_the_self_update_switch_as_a_trailing_fix_line() {
    const OFF_HERE: &str = "Claude Code's own updater is off here (DISABLE_AUTOUPDATER=1)";
    let layout = temp_layout("which-env");
    let claude = crate::vendor_direct::Version::parse("2.1.281")
        .unwrap()
        .build_id();
    let codex = crate::vendor_direct::Version::parse("0.156.0")
        .unwrap()
        .build_id();
    let build = layout.build_dir("claude", claude);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        build.join("bin/claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::activate::install_tools_env(
        &layout,
        &build,
        &[crate::store::ToolName::new("claude").unwrap()],
        crate::activate::Aliases::Off,
        &env,
    )
    .unwrap();
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(claude),
            state: crate::state::vendor_managed("2.1.281", "Anthropic"),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    let shim = layout.shim(&crate::store::ToolName::new("claude").unwrap());
    // The aterm-tab shape: agents/ first, so the twin — laid with its block by
    // `install_tools_env` — is what `claude update` runs here.
    let agents_first = std::env::join_paths([layout.agents_dir(), layout.bin_dir()]).unwrap();
    assert_eq!(
        which_line(&layout, "claude", Some(&agents_first)).unwrap(),
        format!(
            "claude → {} → {} — managed 2.1.281 — Anthropic latest — {OFF_HERE} — `claude \
             update` here runs `aterm pkg update claude`",
            layout
                .agent_shim(&crate::store::ToolName::new("claude").unwrap())
                .display(),
            crate::which(&layout, "claude").unwrap().display()
        )
    );
    assert_eq!(shim_env_fix(&shim, "claude").as_deref(), Some(OFF_HERE));
    // Held and rolled back: the row says why the latest is not what runs, and the
    // fix-line after it claims nothing about the version.
    for why in ["held by local pin", "rolled back from 2.1.282"] {
        record_status(
            &layout,
            "claude",
            crate::ProgramStatus {
                installed_build: Some(claude),
                state: crate::state::vendor_kept("2.1.281", why),
                tree_root: String::new(),
            },
            why.into(),
        );
        let line = which_line(&layout, "claude", Some(&agents_first)).unwrap();
        assert!(
            line.contains(&format!("managed 2.1.281 — {why} — {OFF_HERE} — ")),
            "{line}"
        );
        assert!(!line.contains("latest"), "{line}");
    }
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(claude),
            state: crate::state::vendor_managed("2.1.281", "Anthropic"),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    // NEGATIVE CONTROL — a shell whose PATH has bin/ and not agents/ (any shell
    // outside aterm; the manual's own case): no foreign copy ahead, so the managed arm
    // answers, and `claude update` typed there execs `bin/claude`, which carried no
    // block before 2026-10-02 — the vendor's updater. The sentence keys on the twin, so
    // it is withheld there (an under-claim once a pass lays `bin/` with the block);
    // everything else on the line is unchanged.
    let bin_only = std::env::join_paths([layout.bin_dir()]).unwrap();
    assert_eq!(
        which_line(&layout, "claude", Some(&bin_only)).unwrap(),
        format!(
            "claude → {} → {} — managed 2.1.281 — Anthropic latest — {OFF_HERE}",
            shim.display(),
            crate::which(&layout, "claude").unwrap().display()
        ),
        "no agents/ on this PATH: `claude update` here is the vendor's"
    );
    // A foreign copy ahead on PATH: it runs, without the env — no fix-line about it.
    let local = crate::testdir::dir("atpkg-main-which-env-local-");
    let exe = local.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ahead = std::env::join_paths([local.clone(), layout.bin_dir()]).unwrap();
    // (claude is an AGENT program with its twin laid, so since 2026-09-16 the row
    // is — in an aterm shell — the shell-local `SHADOWED in this shell by …` with the
    // in-place remedy —
    // owner, 2026-09-16: "all the latest and best MUST WORK IN THE SAME TAB" —
    // plus the out-ranked copies line; still no word about self-update.)
    let line = which_line_in(
        &layout,
        "claude",
        Some(&ahead),
        crate::reroute::StubEnv { in_aterm: true },
    )
    .unwrap();
    assert_eq!(
        line,
        format!(
            "claude → {} — {}\nforeign copies on this shell's PATH: {}",
            exe.display(),
            crate::state::agent_shadowed_in_shell(
                claude,
                &exe,
                &layout.agents_dir(),
                false,
                &shell_remedy_command(&layout),
            ),
            exe.display()
        ),
        "the system copy runs without the env: nothing to say about self-update"
    );
    assert!(!line.contains("own updater"), "{line}");
    assert!(
        !line.contains("here runs"),
        "a shadowed row never claims the twin answers the verb: {line}"
    );
    // NEGATIVE CONTROLS on the twin itself, agents/ first on PATH both times: the twin
    // deleted (agents/ unmakeable, a lane that failed) — `bin/claude` runs; and
    // yesterday's LANDING-ONLY twin (laid by the installed app's older atpkg; it
    // resolves exactly where bin/ does, so `agent_twin_current` alone would pass it)
    // — `claude update` there is the vendor's updater until a pass re-lays the
    // block. Neither earns the sentence; the rest of the managed line stands.
    let claude_tool = crate::store::ToolName::new("claude").unwrap();
    let twin = layout.agent_shim(&claude_tool);
    let target = crate::which(&layout, "claude").unwrap();
    std::fs::remove_file(&twin).unwrap();
    let no_twin = which_line(&layout, "claude", Some(&agents_first)).unwrap();
    assert!(
        no_twin.ends_with(&format!(" — Anthropic latest — {OFF_HERE}")),
        "{no_twin}"
    );
    assert!(!no_twin.contains("here runs"), "no twin laid: {no_twin}");
    let landing_only = crate::platform::sh_landing_prelude(
        "claude",
        &layout.prefix,
        &layout.landing_marker(&claude_tool),
        &crate::stub::co_located_atpkg_path(),
    );
    crate::platform::install_twin_to_env(&twin, &target, &env, &landing_only).unwrap();
    assert!(
        agent_twin_current(&layout, "claude", claude),
        "the landing-only twin resolves where bin/ does — target equality is not enough"
    );
    assert!(!agent_twin_intercepts(&layout, "claude", claude));
    let stale = which_line(&layout, "claude", Some(&agents_first)).unwrap();
    assert!(
        stale.ends_with(&format!(" — Anthropic latest — {OFF_HERE}")),
        "{stale}"
    );
    assert!(
        !stale.contains("here runs"),
        "a twin without the block: {stale}"
    );
    // A plain shim (no env declared) earns no fix-line — and still the sentence
    // (`install_tools` re-lays the twin, block and all, through `reconcile_agents`).
    crate::activate::install_tools(
        &layout,
        &build,
        &[crate::store::ToolName::new("claude").unwrap()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    assert!(agent_twin_intercepts(&layout, "claude", claude));
    assert_eq!(shim_env_fix(&shim, "claude"), None);
    let plain = which_line(&layout, "claude", Some(&agents_first)).unwrap();
    assert!(!plain.contains("own updater"), "{plain}");
    assert!(
        plain
            .ends_with(" — Anthropic latest — `claude update` here runs `aterm pkg update claude`"),
        "{plain}"
    );
    // codex: no shim_env at all, so no fix-line — and the sentence, its own verb.
    let codex_build = layout.build_dir("codex", codex);
    std::fs::create_dir_all(codex_build.join("bin")).unwrap();
    std::fs::write(codex_build.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        codex_build.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_tools(
        &layout,
        &codex_build,
        &[crate::store::ToolName::new("codex").unwrap()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    record_status(
        &layout,
        "codex",
        crate::ProgramStatus {
            installed_build: Some(codex),
            state: crate::state::vendor_kept("0.156.0", "held by local pin"),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    let codex_shim = layout.shim(&crate::store::ToolName::new("codex").unwrap());
    assert_eq!(shim_env_fix(&codex_shim, "codex"), None);
    assert_eq!(
        which_line(&layout, "codex", Some(&agents_first)).unwrap(),
        format!(
            "codex → {} → {} — managed 0.156.0 — held by local pin — `codex update` here \
             runs `aterm pkg update codex`",
            layout
                .agent_shim(&crate::store::ToolName::new("codex").unwrap())
                .display(),
            crate::which(&layout, "codex").unwrap().display()
        )
    );
    // A non-agent program: no sentence, whatever its env.
    let ay_build = layout.build_dir("ay", 18);
    std::fs::create_dir_all(ay_build.join("bin")).unwrap();
    std::fs::write(ay_build.join("bin/ay"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        ay_build.join("bin/ay"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::activate::install_tools_env(
        &layout,
        &ay_build,
        &[crate::store::ToolName::new("ay").unwrap()],
        crate::activate::Aliases::Off,
        &env,
    )
    .unwrap();
    let ay_line = which_line(&layout, "ay", Some(&agents_first)).unwrap();
    assert!(ay_line.contains("its own updater is off here"), "{ay_line}");
    assert!(!ay_line.contains("here runs"), "{ay_line}");
    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The predicate behind the `which` sentence, over the bytes every twin renderer
/// lays and the twins older clients laid (pure, cfg-free, so the Windows shape is
/// pinned from here like every `.cmd` test): the `sh` twin WITH the block carries the
/// hand-over, and so does the older block-then-landing twin; the landing-only `sh`
/// twin, the `bin/` shim and the `.cmd` twin — whose self-update prelude renders the
/// empty string, so on Windows `claude update` is the vendor's — do not, today's
/// (no prelude) or an older one's (the landing prelude). That last case is what keeps
/// `aterm pkg which claude` from claiming `here runs …` on a Windows host by
/// construction (review, 2026-09-19).
#[test]
fn the_hand_over_predicate_reads_the_sh_block_and_never_the_cmd_twin_or_bin() {
    use std::path::Path;
    let prefix = Path::new("/Users//u/Library/Application Support/aterm/pkg");
    let marker = Path::new("/Users//u/Library/Application Support/aterm/pkg/landing/claude");
    let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    let target = Path::new(
        "/Users//u/Library/Application Support/aterm/pkg/store/claude/2026091902/bin/claude",
    );
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let verbs = crate::selfupdate::verbs_of("claude");
    let with_block = crate::platform::sh_selfupdate_prelude("claude", prefix, atpkg, verbs);
    assert!(twin_carries_hand_over(
        crate::platform::sh_shim_content_twin(target, &env, None, &with_block).as_bytes()
    ));
    let landing = crate::platform::sh_landing_prelude("claude", prefix, marker, atpkg);
    let mut block_then_landing = with_block.clone();
    block_then_landing.push_str(&landing);
    assert!(twin_carries_hand_over(
        crate::platform::sh_shim_content_twin(target, &env, None, &block_then_landing).as_bytes()
    ));
    assert!(!twin_carries_hand_over(
        crate::platform::sh_shim_content_twin(target, &env, None, &landing).as_bytes()
    ));
    assert!(!twin_carries_hand_over(
        crate::platform::sh_shim_content_env(target, &env).as_bytes()
    ));
    let cmd_prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
    let cmd_marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
    let cmd_atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
    let cmd_target = Path::new(
        "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
    );
    let cmd_prelude =
        crate::platform::cmd_selfupdate_prelude("claude", cmd_prefix, cmd_atpkg, verbs);
    assert!(
        !twin_carries_hand_over(
            crate::platform::cmd_shim_content_twin(cmd_target, &env, &cmd_prelude).as_bytes()
        ),
        "the .cmd twin renders no block, so the sentence is never claimed on Windows"
    );
    let mut older_cmd_prelude = cmd_prelude.clone();
    older_cmd_prelude.push_str(&crate::platform::cmd_landing_prelude(
        "claude", cmd_prefix, cmd_marker, cmd_atpkg,
    ));
    assert!(!twin_carries_hand_over(
        crate::platform::cmd_shim_content_twin(cmd_target, &env, &older_cmd_prelude).as_bytes()
    ));
    assert!(!twin_carries_hand_over(b""));
}

/// SYSTEM SATISFACTION: a `system = "gh"` member with `gh` on PATH outside the prefix is
/// satisfied (recorded, announced, not planned); a managed copy it had is retired so the
/// system binary wins; the managed `bin/` on PATH never counts; a member without the key
/// is never satisfied, whatever PATH holds.
#[cfg(unix)]
#[test]
fn a_system_install_satisfies_a_default_set_member_and_retires_the_managed_copy() {
    let layout = temp_layout("system");
    let index = index_of(&[("ay", None), ("gh", Some("gh"))]);
    let sys = layout
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-main-system-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sys);
    std::fs::create_dir_all(&sys).unwrap();
    for name in ["gh", "ay"] {
        let exe = sys.join(name);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths([layout.bin_dir(), sys.clone()]).unwrap();
    let wanted = names(&["ay", "gh"]);
    // 1. Nothing managed yet: gh is satisfied, ay (no `system` key) is not.
    let mut installed = std::collections::BTreeMap::new();
    assert_eq!(
        reconcile_system_satisfied_with(&layout, &index, &wanted, &mut installed, Some(&path)),
        vec!["gh".to_string()]
    );
    let row = crate::status::read(&layout).unwrap().programs["gh"].clone();
    assert_eq!(row.installed_build, None);
    assert_eq!(
        row.state,
        crate::state::system(&sys.join("gh"), None),
        "the row is the canonical system state, naming the path, with no retirement yet"
    );
    assert_eq!(layout.retired_date("gh"), None);
    // 2. A managed copy exists: it is retired and the plan no longer counts it installed.
    let build = layout.build_dir("gh", 7);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/gh"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(build.join("bin/gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::activate::install_shims(
        &layout,
        &build,
        &["gh".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    let mut installed = crate::active_builds(&layout);
    assert_eq!(
        installed.get("gh"),
        Some(&7),
        "PRECONDITION: the managed copy is live"
    );
    assert_eq!(
        reconcile_system_satisfied_with(&layout, &index, &wanted, &mut installed, Some(&path)),
        vec!["gh".to_string()]
    );
    assert!(!installed.contains_key("gh"));
    assert!(
        crate::which(&layout, "gh").is_none(),
        "the managed shim is gone"
    );
    assert!(!build.exists(), "the managed store tree is gone");
    // The retirement is DATED: the marker holds today, and the row says so.
    let date = layout
        .retired_date("gh")
        .expect("a retired managed copy leaves a dated marker");
    assert_eq!(date.len(), 10, "{date}");
    let row = crate::status::read(&layout).unwrap().programs["gh"].clone();
    assert_eq!(
        row.state,
        crate::state::system(&sys.join("gh"), Some(&date))
    );
    assert_eq!(
        crate::state::system_retired(&row.state),
        Some(date.as_str())
    );
    // `which` reads the same facts back, in the same words.
    assert_eq!(
        which_line(&layout, "gh", Some(&path)).unwrap(),
        format!(
            "gh → {} — {}",
            sys.join("gh").display(),
            crate::state::system(&sys.join("gh"), Some(&date))
        )
    );
    assert!(
        !layout.removed().exists(),
        "NOT a removal on purpose: no `removed` marker, so the member returns if the \
         system binary goes away"
    );
    // 3. Only the managed bin/ on PATH: not satisfied — the member stays wanted.
    let managed_only = std::env::join_paths([layout.bin_dir()]).unwrap();
    let mut none = std::collections::BTreeMap::new();
    assert!(
        reconcile_system_satisfied_with(&layout, &index, &wanted, &mut none, Some(&managed_only))
            .is_empty()
    );
    assert!(reconcile_system_satisfied_with(&layout, &index, &wanted, &mut none, None).is_empty());
    let _ = std::fs::remove_dir_all(&sys);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A refused vendor row is a fact about THIS machine's install attempt (the row was
/// signed and reached the client), so it lands on the record like any other stage fault.
#[test]
fn a_refused_vendor_row_is_recorded_as_an_error_state() {
    assert_eq!(
        failed_install_state(&crate::FlowError::VendorRefused("url host is evil".into())),
        Some(String::from(
            "error: artifact row refused: url host is evil"
        ))
    );
}

/// The triple this client reports for itself is one of the six the schema serves —
/// a row can only ever be selected under a spelling `TARGETS` carries.
#[test]
fn the_current_triple_is_a_schema_target() {
    assert!(
        crate::manifest::TARGETS.contains(&current_triple()),
        "{} is not in TARGETS",
        current_triple()
    );
}

/// A verdict the AUTHORITY never gave must not become a resident of the record.
///
/// `NotReachable` means the signed index resolved, verified, and does not name the
/// token — a fact about the request, not about this store. Recording it as a program
/// state invents a program, and `atpkg doctor` reads every such row back as a missing
/// member of the toolset.
#[test]
fn a_name_the_index_does_not_name_is_never_recorded() {
    assert_eq!(
        failed_install_state(&crate::FlowError::NotReachable("--help".into(), vec![])),
        None,
        "the index does not name it, so there is no program to remember"
    );
    assert_eq!(
        failed_install_state(&crate::FlowError::Linked("ay".into())),
        Some(String::from("dev-linked (skipped)")),
        "a dev-linked program is a benign hard-skip (§13), and still says so"
    );
    // THE OFFLINE FAMILY, all raised before the program lookup ever happens. Recording
    // any of these against a program says the program is broken; what happened is that
    // the machine could not reach the channel.
    for env in [
        crate::FlowError::NoIndex,
        crate::FlowError::Unreachable("dns".into()),
        crate::FlowError::Stale,
        crate::FlowError::VendorUnreachable("Anthropic's release channel was not reached".into()),
    ] {
        assert_eq!(
            failed_install_state(&env),
            None,
            "{env} is a fact about the network, not about any program"
        );
    }
    // …while a genuine per-program fact about THIS machine still lands on the record:
    // no row for this target is the canonical `unavailable on` STATE (never an error).
    assert_eq!(
        failed_install_state(&crate::FlowError::NoArtifact("aarch64".into())),
        Some(crate::state::unavailable("aarch64", "")),
        "no build for this target is a real, durable, non-error fact about the program"
    );
    assert_eq!(
        failed_install_state(&crate::FlowError::PkgVerify),
        Some(format!("error: {}", crate::FlowError::PkgVerify)),
        "a manifest that failed its signature check is a fault worth remembering"
    );
}

/// Put `program` genuinely LIVE in `layout` at `build` — real build tree, real shims,
/// real channel pointer — so `active_builds` reports it the way it does on a machine.
/// A stubbed record would not exercise the thing under test.
fn make_live(layout: &crate::store::Layout, program: &str, build: u64) {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let t = crate::store::ToolName::new(program).unwrap();
    std::fs::write(dir.join("bin").join(t.exe_file()), b"#!/bin/true\n").unwrap();
    crate::activate::install_shims(
        layout,
        &dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
}

/// ROLL FORWARD (Phase 3): a pass killed between a `current` flip and its shims left a
/// complete build no shim runs; the next pass's edge lays its shims from the build and
/// its sidecars — no fetcher exists here, so nothing can be staged or downloaded. A
/// program the machine removed, and an index program whose group membership no cached
/// index can vouch for (a lone member of a tuple must not be split off), are left to
/// the pass; a store already consistent rolls nothing.
#[test]
fn an_interrupted_activation_is_rolled_forward_without_a_fetch() {
    let layout = temp_layout("roll-forward");
    let v281 = vendor_version("2.1.281").build_id();
    let v282 = vendor_version("2.1.282").build_id();
    make_live(&layout, "claude", v281);
    make_live(&layout, "ay", 18);
    assert!(
        roll_forward_interrupted_activations(&layout, || None).is_empty(),
        "consistent: nothing to finish"
    );
    // The kill: 2.1.282 staged complete and `current` flipped, its shims never laid.
    flip_only(&layout, "claude", v282);
    flip_only(&layout, "ay", 19);
    assert_eq!(crate::active_builds(&layout).get("claude"), Some(&v281));
    assert_eq!(
        roll_forward_interrupted_activations(&layout, || None),
        vec!["claude".to_string()],
        "the vendor program rolls forward; ay's group is unknown with no cached index"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        active.get("claude"),
        Some(&v282),
        "its shims run the new build"
    );
    assert_eq!(active.get("ay"), Some(&18), "left to the pass");
    assert!(
        agent_twin_current(&layout, "claude", v282),
        "the twin follows the shim"
    );
    assert!(
        roll_forward_interrupted_activations(&layout, || None).is_empty(),
        "converged"
    );
    // A removed program is never revived by it.
    flip_only(&layout, "claude", vendor_version("2.1.283").build_id());
    std::fs::write(layout.removed(), "claude\n").unwrap();
    assert!(roll_forward_interrupted_activations(&layout, || None).is_empty());
    assert_eq!(crate::active_builds(&layout).get("claude"), Some(&v282));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `program`'s build `build` staged complete and its `current` flipped, its shims never
/// laid — the state a pass killed between the two leaves.
fn flip_only(layout: &crate::store::Layout, program: &str, build: u64) {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("bin").join(program),
        format!("#!/bin/true\n# {build}\n"),
    )
    .unwrap();
    std::fs::set_permissions(
        dir.join("bin").join(program),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
}

/// THE ROW FOLLOWS THE ROLL-FORWARD, AND AN OLD ROOT NEVER ATTESTS A NEW BUILD. ay 18 was
/// live with its signed root; a pass flipped `current` to 19 and was killed before the
/// shims. The next pass's edge rolls ay forward and records 19 with no root, its apply
/// finds ay up to date, and its root recovery re-derives 19's signed root — so `verify`
/// matches. The same holds when the row was LOST (a pass killed before its row reached
/// the disk): the row still naming 18 and 18's root is never carried onto 19, which
/// `verify` read as drift on untouched bytes.
#[cfg(unix)]
#[test]
fn a_rolled_forward_build_is_attested_by_its_own_root_never_the_old_one() {
    use crate::vendor_direct::lane::world::{self, Fake};
    for (label, lose_the_row) in [("roll-root", false), ("roll-root-lost", true)] {
        let layout = world::layout(label);
        let f = Fake::default();
        // The world's index pins ay at 18 (ungrouped): here the live build is 17.
        *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
        make_live(&layout, "ay", 17);
        let root17 = crate::tree::tree_root(&layout.build_dir("ay", 17)).unwrap();
        record_status(
            &layout,
            "ay",
            crate::ProgramStatus {
                installed_build: Some(17),
                state: crate::state::managed(17, 43),
                tree_root: root17.clone(),
            },
            String::from("installed ay build 17"),
        );
        flip_only(&layout, "ay", 18);
        let root18 = crate::tree::tree_root(&layout.build_dir("ay", 18)).unwrap();
        assert_ne!(root17, root18);
        f.publish_pkg_root("ay", 18, current_triple(), &root18);
        let index = || {
            crate::resolve_verified_index(
                &f,
                &layout,
                &crate::sig::testkit::anchor(),
                vendor_floor(),
                world_now(),
            )
            .ok()
        };
        assert_eq!(
            roll_forward_interrupted_activations(&layout, index),
            vec!["ay".to_string()],
            "{label}"
        );
        let row = crate::status::read(&layout).unwrap().programs["ay"].clone();
        assert_eq!(row.installed_build, Some(18), "{label}: the row follows");
        assert!(row.tree_root.is_empty(), "{label}: never 17's root");
        if lose_the_row {
            record_status(
                &layout,
                "ay",
                crate::ProgramStatus {
                    installed_build: Some(17),
                    state: crate::state::managed(17, 43),
                    tree_root: root17.clone(),
                },
                String::from("installed ay build 17"),
            );
        }
        let cfg = crate::config::PackagesConfig::default();
        let trust = world::trust();
        let seams = world_seams(&trust);
        let installed = crate::active_builds(&layout);
        assert_eq!(installed.get("ay"), Some(&18));
        assert_eq!(
            run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
            0,
            "{label}"
        );
        let row = crate::status::read(&layout).unwrap().programs["ay"].clone();
        assert_eq!(row.installed_build, Some(18), "{label}");
        assert_eq!(row.tree_root, root18, "{label}: 18's own signed root");
        assert_eq!(
            crate::verify::verify_program(&layout, "ay"),
            crate::verify::VerifyOutcome::Match { build: 18 },
            "{label}"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}

/// Roll-forward activates nothing the normal path would refuse: a PINNED program, a
/// build the latest verified index YANKED, and — after an `atpkg rollback` — never the
/// build rolled off: the undo flips `current` before it re-lays the shims, so the one
/// divergence it can leave points at the PRIOR build.
#[cfg(unix)]
#[test]
fn roll_forward_never_activates_a_pinned_yanked_or_rolled_off_build() {
    use crate::vendor_direct::lane::world::{self, Fake};
    let layout = world::layout("roll-gates");
    let f = Fake::default();
    make_live(&layout, "ay", 17);
    flip_only(&layout, "ay", 18);
    let index = |yanked: &[&str]| {
        *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", yanked));
        crate::resolve_verified_index(
            &f,
            &layout,
            &crate::sig::testkit::anchor(),
            vendor_floor(),
            world_now(),
        )
        .ok()
    };
    assert!(
        roll_forward_interrupted_activations(&layout, || index(&["ay@18"])).is_empty(),
        "yanked"
    );
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    assert!(
        roll_forward_interrupted_activations(&layout, || index(&[])).is_empty(),
        "pinned"
    );
    assert_eq!(crate::active_builds(&layout).get("ay"), Some(&17));
    crate::pin::set_pinned(&layout, "ay", false).unwrap();
    assert_eq!(
        roll_forward_interrupted_activations(&layout, || index(&[])),
        vec!["ay".to_string()]
    );
    // `atpkg rollback ay` off 18 back to 17 (its undo's order is pinned in flow.rs).
    crate::flow::rollback(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        "stable",
        "ay",
        &[],
        vendor_floor(),
        world_now(),
    )
    .unwrap();
    assert_eq!(crate::active_builds(&layout).get("ay"), Some(&17));
    assert!(
        roll_forward_interrupted_activations(&layout, || index(&[])).is_empty(),
        "the rolled-off build stays off"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE BUILD A PASS KEEPS BELOW THE LIVE ONE IS THE BUILD `aterm pkg rollback` LANDS ON.
/// ay 16, 17 and 18 retained, 18 live, and the channel yanks 17: the pass's closing gc
/// kept the yanked 17 as "the rollback" by its number alone and reclaimed 16, so the
/// verb then refused ("ay 17 is revoked — ay 18 stays") on a store that had a build it
/// could land on until that pass.
#[test]
fn the_pass_keeps_the_build_a_rollback_lands_on() {
    use crate::vendor_direct::lane::world::{self, Fake};
    let layout = world::layout("pass-rollback-target");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &["ay@17"]));
    for b in [16, 17] {
        seed_ready_build(&layout, "ay", b);
    }
    make_live(&layout, "ay", 18);
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let installed = crate::active_builds(&layout);
    run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    let landed = crate::flow::rollback(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        "stable",
        "ay",
        &[],
        vendor_floor(),
        world_now(),
    );
    assert_eq!(
        landed.map(|r| r.to_build).map_err(|e| e.to_string()),
        Ok(16)
    );
    assert!(
        !layout.build_dir("ay", 17).exists(),
        "the pass's gc ran, and reclaimed the revoked build"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A FAILED INSTALL UNINSTALLS NOTHING — so it must not report that it did.
///
/// An upgrade that dies at download/stage/activate leaves the previous build activated,
/// shimmed and working. Every failure arm nonetheless wrote `installed_build: None` and
/// `tree_root: ""`, which says the program is gone AND unattested. The second is
/// destructive: `crate::verify` fails closed on an empty root, so ONE failed upgrade
/// turned `atpkg verify` into a permanent "cannot verify" for bytes that were never
/// touched — recoverable only by reinstalling gigabytes to re-record a root that had
/// been correct the whole time.
#[test]
fn a_failed_install_keeps_the_live_build_and_its_attestation() {
    let layout = temp_layout("failure-row");
    make_live(&layout, "ay", 18);
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: "active".into(),
            tree_root: "0fda8eaefbfae96ac95ffb83afce208e".into(),
        },
        "up to date".into(),
    );

    // An upgrade to 20 dies mid-flight.
    let row = failure_row(&layout, "ay", String::from("error: download failed"));
    assert_eq!(
        row.installed_build,
        Some(18),
        "build 18 is still activated and shimmed; the record must not claim otherwise"
    );
    assert_eq!(
        row.tree_root, "0fda8eaefbfae96ac95ffb83afce208e",
        "the signed attestation for untouched bytes survives a failed upgrade"
    );
    assert_eq!(
        row.state, "error: download failed",
        "the failure is still reported"
    );

    // …while a program that was never installed still records nothing to preserve.
    let fresh = failure_row(&layout, "clean", String::from("error: download failed"));
    assert_eq!(fresh.installed_build, None);
    assert!(fresh.tree_root.is_empty());

    // THE RECORD DISAGREES WITH THE STORE — the case the two fields are read from
    // different sources, and the one that makes stapling them together dangerous. The
    // store is now live at 20 while the record still says 18/root18. Taking the build
    // from the store and the root from the record would produce "build 20, attested by
    // build 18's signature", which DISARMS verify's `BuildMismatch` guard and lands in
    // the tree comparison: `Drift` (a tampering accusation against untouched bytes) or,
    // worse, `Match` — affirming a build no signature ever covered.
    make_live(&layout, "ay", 20);
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(20),
        "precondition: the store moved on and the record has not caught up"
    );
    let stitched = failure_row(&layout, "ay", String::from("error: download failed"));
    assert_eq!(
        stitched.installed_build,
        Some(20),
        "the store is ground truth for what is live"
    );
    assert!(
        stitched.tree_root.is_empty(),
        "an attestation captured for build 18 must NOT be stapled onto build 20 — an \
         honest `NoSignedRoot` beats a false `Drift`, and beats a false `Match` by \
         considerably more"
    );

    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A TOMBSTONED PROGRAM KEEPS ITS ATTESTATION. Its shims were replaced by failing stubs
/// so `active_builds` goes silent, but its bytes and their signed root sit untouched in
/// the store — and the build-agreement rule above must not read that silence as
/// disagreement and throw the root away on the strength of a yanked pin.
#[test]
fn a_tombstoned_program_does_not_lose_its_signed_root() {
    let layout = temp_layout("tombstone-root");
    record_status(
        &layout,
        "trust",
        crate::ProgramStatus {
            installed_build: Some(6459),
            state: "active".into(),
            tree_root: "abc123".into(),
        },
        "up to date".into(),
    );
    // No shims at all: exactly what `active_builds` sees for a tombstoned program.
    assert_eq!(crate::active_builds(&layout).get("trust").copied(), None);

    let row = failure_row(&layout, "trust", String::from("tombstoned: pin yanked"));
    assert_eq!(
        row.tree_root, "abc123",
        "nothing contradicts the recorded root, so it survives"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A COMPLETE build dir with an executable `bin/<program>` and its ready marker — the
/// shape `list_installed` reports — for the repair tests below.
fn seed_ready_build(layout: &crate::store::Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join(program);
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    dir
}

/// Lay `program`'s shims at `dir` and point both `current` links at it.
fn activate_seeded(layout: &crate::store::Layout, program: &str, dir: &Path) {
    crate::activate::install_shims(
        layout,
        dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(layout, dir).unwrap();
}

/// Unlink's restore tells "no installed build" (off PATH) apart from an installed
/// build whose commands the link never took over (they still run): only the first
/// sends the reader to `aterm pkg install`.
#[test]
fn unlink_restore_tells_no_build_from_nothing_taken_over() {
    let l = temp_layout("restore-kinds");
    let owned = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        restore_installed_shims(&l, "foo", Some(&owned(&["foo"]))).unwrap(),
        Restored::NoBuild
    );
    let dir = seed_ready_build(&l, "foo", 1);
    activate_seeded(&l, "foo", &dir);
    assert_eq!(
        restore_installed_shims(&l, "foo", Some(&owned(&["foo-dev"]))).unwrap(),
        Restored::Nothing,
        "the link took over a name the installed build does not have"
    );
    assert_eq!(
        restore_installed_shims(&l, "foo", Some(&owned(&["foo"]))).unwrap(),
        Restored::Laid(1)
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// REPAIR NEVER REVIVES A REVOKED BUILD (audit K2, 2026-09-12).
///
/// A tombstone keeps the revoked build's tree, marker and `current` links; only its
/// shim turns into a failing notice, which resolves to nothing. Repair used to read
/// that silence as "no exposes recorded", fall back to the program's name and lay a
/// live shim over the tombstone — making a yanked/below-floor build runnable again.
/// Refused both on the recorded `tombstoned:` row and on the tombstone shim alone.
#[test]
fn repair_does_not_revive_a_tombstoned_build() {
    for with_status_row in [true, false] {
        let label = if with_status_row {
            "repair-tomb-row"
        } else {
            "repair-tomb-shim"
        };
        let l = temp_layout(label);
        let d17 = seed_ready_build(&l, "ay", 17);
        activate_seeded(&l, "ay", &d17);
        if with_status_row {
            record_status(
                &l,
                "ay",
                crate::ProgramStatus {
                    installed_build: Some(17),
                    state: "tombstoned: pin yanked/below floor".into(),
                    tree_root: String::new(),
                },
                "tombstoned".into(),
            );
        }
        let ay = crate::store::ToolName::new("ay").unwrap();
        crate::activate::install_tombstone_shim(&l, &ay).unwrap();
        assert!(
            crate::platform::resolve_shim(&l.shim(&ay)).is_none(),
            "precondition: the tombstone forwards nowhere"
        );

        let (_, _, revoked, _) = relay_shims(&l);

        assert_eq!(revoked, ["ay@17"], "{label}: named as still revoked");
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&ay)),
            None,
            "{label}: repair laid a live shim over the tombstone"
        );
        assert!(
            std::fs::read_to_string(l.shim(&ay))
                .unwrap()
                .contains("was yanked/revoked"),
            "{label}: the failing notice is still what `ay` runs"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

/// A LAY THAT FAILS REACHES THE EXIT CODE (audit 2026-09-12). Repair printed a refused
/// lay and then said "done", exit 0, so a scripted `repair && …` could not tell fixed
/// from refused. Every program whose shims could not be re-laid now comes back in
/// the fourth slot and `run_repair` exits 1 on it. Forced here the way any lay fails: an
/// unwritable `bin/`.
#[cfg(unix)]
#[test]
fn a_shim_that_cannot_be_relaid_is_named_and_not_swallowed() {
    let l = temp_layout("repair-failed-lay");
    let d17 = seed_ready_build(&l, "ay", 17);
    activate_seeded(&l, "ay", &d17);
    let ay = crate::store::ToolName::new("ay").unwrap();
    // The seeded activation already laid `ay`; a lay that finds its shim in place and
    // identical writes nothing, so take it away — and a read-only `bin/` is no
    // obstacle either (the lay re-asserts the directory's mode). A DIRECTORY where
    // the shim goes is: no rename lands a file over it.
    std::fs::remove_file(l.shim(&ay)).unwrap();
    std::fs::create_dir(l.shim(&ay)).unwrap();

    let report = relay_shims(&l);

    let (_, _, _, failed) = &report;
    assert_eq!(
        failed.len(),
        1,
        "one program could not be re-laid: report={report:?}"
    );
    assert!(failed[0].starts_with("ay@17"), "{failed:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The command step's two arms both reach the verdict: a copy the move could not
/// make, and — from an atpkg that is not the installed app's — every copy still in
/// `~/.local/bin`, each a leftover; a clean move leaves none.
#[cfg(unix)]
#[test]
fn every_copy_the_command_step_leaves_is_a_leftover() {
    let failed = crate::hooks::AsideOutcome {
        moved: Vec::new(),
        failed: vec![(
            PathBuf::from("/home/u/.local/bin/aterm"),
            "it changed while it was being moved".to_string(),
        )],
    };
    assert_eq!(
        repair_copied_commands_with(Ok(failed), None),
        vec!["/home/u/.local/bin/aterm is still a file".to_string()]
    );
    assert!(
        repair_copied_commands_with(Ok(crate::hooks::AsideOutcome::default()), None).is_empty()
    );

    let home = crate::testdir::dir("atpkg-repair-left-");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aterm"), b"an old copy").unwrap();
    let left =
        repair_copied_commands_with(Err("this atpkg is not inside an app bundle"), Some(&home));
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left[0].ends_with("/.local/bin/aterm is still a file"),
        "{left:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A copied command the first step could not move keeps repair from "done": with
/// nothing installed and with a healthy store alike, the verdict is exit 1, and
/// the same store with nothing left over is done.
#[cfg(unix)]
#[test]
fn a_copied_command_left_in_place_keeps_repair_from_saying_done() {
    let leftover = ["/home/u/.local/bin/aterm is still a file".to_string()];
    let empty = temp_layout("repair-leftover-empty");
    assert_eq!(repair_store(&empty, &leftover), ExitCode::from(1));
    assert_eq!(repair_store(&empty, &[]), ExitCode::SUCCESS);

    let l = temp_layout("repair-leftover-healthy");
    let d19 = seed_ready_build(&l, "ay", 19);
    activate_seeded(&l, "ay", &d19);
    let _ = crate::provenance::test_bind::faked();
    assert_eq!(
        repair_store(&l, &[]),
        ExitCode::SUCCESS,
        "healthy and nothing left"
    );
    assert_eq!(
        repair_store(&l, &leftover),
        ExitCode::from(1),
        "healthy, but a copy is still in place"
    );
    let _ = std::fs::remove_dir_all(&empty.prefix);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// REPAIR IS THE HEAL (2026-09-24). The `<build>.tracked-install` records the retired
/// untracked lanes wrote — one beside a superseded build, one beside the live one —
/// made repair exit 1 for ever on m7. Repair now clears the tag, sweeps both records,
/// and exits 0 because nothing stays tagged.
///
/// The one door test on the REAL heal ([`crate::provenance::test_bind::real`]): it
/// asserts the files come out clean, which only a launchd job can make them.
#[cfg(unix)]
#[test]
fn repair_clears_the_tag_and_sweeps_the_retired_records() {
    let _real = crate::provenance::test_bind::real();
    let _ = crate::provenance::test_bind::faked();
    let l = temp_layout("repair-tag-records");
    let d18 = seed_ready_build(&l, "ay", 18);
    let d19 = seed_ready_build(&l, "ay", 19);
    activate_seeded(&l, "ay", &d19);
    let records = [
        d18.with_file_name("18.tracked-install"),
        d19.with_file_name("19.tracked-install"),
    ];
    for record in &records {
        std::fs::write(record, "tracked-install v1\nwhy=the untracked lane ran\n").unwrap();
    }
    // Whatever this test process wrote is tagged exactly when it is tracked; the heal
    // clears it either way.
    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);
    for record in &records {
        assert!(!record.exists(), "{} swept", record.display());
    }
    assert!(
        crate::provenance::scan_roots(
            &crate::provenance::store_roots(&l),
            crate::provenance::PROVENANCE_XATTR
        )
        .carriers
        .is_empty(),
        "nothing repair reaches carries the tag"
    );
    assert!(
        crate::provenance::test_bind::faked().is_empty(),
        "the real heal ran, not the stand-in"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// REPAIR HEALS AN EMPTY STORE TOO (2026-09-24). With nothing installed, `bin/`'s
/// pending stubs and `reroute/`'s stubs still stand, and doctor counts a tagged one with
/// `aterm pkg repair` as its fix — so the "no programs installed" answer runs the heal
/// like every other, where it used to return before it and leave the tag for good.
#[cfg(unix)]
#[test]
fn repair_of_an_empty_store_still_heals() {
    let l = temp_layout("repair-empty-heal");
    std::fs::create_dir_all(l.bin_dir()).unwrap();
    std::fs::write(l.bin_dir().join("ty"), "#!/bin/sh\n").unwrap();
    let _ = crate::provenance::test_bind::faked();
    assert!(
        crate::list_installed(&l).is_empty(),
        "the fixture installs nothing"
    );
    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);
    let heals = crate::provenance::test_bind::faked();
    assert_eq!(heals.len(), 1, "one store heal: {heals:?}");
    assert!(
        heals[0].0.contains(&l.bin_dir()),
        "over the stubs' bin/: {heals:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// REPAIR PUTS BACK A PROGRAM'S OWN-NAME SHIM WHEN ONLY IT IS GONE. The m3 end state
/// (2026-09-23): ty 3007 ships `bin/ty` and `bin/tla`, and only `tla` (+`alab-tla`)
/// survived a trust rollback and the prune after it. Repair derived ty's exposes from
/// the survivors — `[tla]` — so `ty` never came back. Control: a program whose own-name
/// shim is PRESENT as a dev link into a checkout keeps that link.
#[cfg(unix)]
#[test]
fn repair_lays_a_lost_own_name_shim_beside_the_survivors() {
    let l = temp_layout("repair-own-name");
    let d = seed_ready_build(&l, "ty", 3007);
    let tla = d.join("bin").join("tla");
    std::fs::write(&tla, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tla, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::activate::install_shims(
        &l,
        &d,
        &["ty".to_string(), "tla".to_string()],
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    crate::activate::activate_build(&l, &d).unwrap();
    let ty = crate::store::ToolName::new("ty").unwrap();
    std::fs::remove_file(l.shim(&ty)).unwrap();
    std::fs::remove_file(l.shim(&ty.alias().unwrap())).unwrap();

    let mut exposes = repair_exposes(&l, "ty", &d);
    exposes.sort();
    assert_eq!(exposes, ["tla", "ty"], "the lost own name is re-laid");
    let (relaid, _, _, failed) = relay_shims(&l);
    assert_eq!((relaid, failed.len()), (1, 0));
    for name in ["ty", "alab-ty", "tla", "alab-tla"] {
        let target =
            crate::platform::resolve_shim(&l.shim(&crate::store::ToolName::new(name).unwrap()))
                .unwrap_or_else(|| panic!("{name} is on PATH after repair"));
        assert!(target.starts_with(&d), "{name}: {}", target.display());
    }

    // Control: the own-name shim is a dev link into a checkout — present, so untouched.
    let checkout = l.prefix.join("checkout-bin");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("ty"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::remove_file(l.shim(&ty)).unwrap();
    crate::platform::install_shim_env(
        &checkout,
        &ty,
        &l.shim(&ty),
        &crate::shim_env::ShimEnv::default(),
    )
    .unwrap();
    assert_eq!(
        repair_exposes(&l, "ty", &d),
        ["tla"],
        "a present shim is not claimed"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The m3 shape (2026-09-23): ty 3007, shipping `bin/ty` and `bin/tla`, laid with its
/// `alab-` aliases and activated — then `bin/ty` and `alab-ty` lost, and with `all`
/// every shim of it. Returns the layout and the build dir.
#[cfg(unix)]
fn seed_ty_with_lost_shim(label: &str, all: bool) -> (crate::testdir::ScratchLayout, PathBuf) {
    let l = temp_layout(label);
    let d = l.build_dir("ty", 3007);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    for tool in ["ty", "tla"] {
        let exe = d.join("bin").join(tool);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::store::mark_build_ready(&d).unwrap();
    crate::activate::install_shims(
        &l,
        &d,
        &["ty".to_string(), "tla".to_string()],
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    crate::activate::activate_build(&l, &d).unwrap();
    let lost: &[&str] = if all { &["ty", "tla"] } else { &["ty"] };
    for name in lost {
        let tool = crate::store::ToolName::new(name).unwrap();
        std::fs::remove_file(l.shim(&tool)).unwrap();
        std::fs::remove_file(l.shim(&tool.alias().unwrap())).unwrap();
    }
    (l, d)
}

/// A `PATH` that holds nothing: one empty directory under the layout.
#[cfg(unix)]
fn empty_path(l: &crate::store::Layout) -> std::ffi::OsString {
    let dir = l.prefix.join("empty-path");
    std::fs::create_dir_all(&dir).unwrap();
    dir.into_os_string()
}

/// A prefix from before the per-program `current` link still answers a lost shim —
/// from the build the program's surviving shims point into — and a name with no
/// store directory at all is refused before that walk of every shim in `bin/`, which
/// the `aterm <word>` front door would otherwise pay for every unknown word.
#[cfg(unix)]
#[test]
fn a_lost_shim_without_a_current_link_answers_from_the_surviving_shims() {
    let (l, d) = seed_ty_with_lost_shim("lost-shim-no-current", false);
    std::fs::remove_file(l.program_current("ty")).unwrap();
    let found = live_without_shim(&l, "ty").expect("tla's shim still names build 3007");
    assert_eq!(
        (found.build, found.binary),
        (3007, d.join("bin").join("ty"))
    );
    // With every shim lost too, nothing local names the build: no answer, rather
    // than a guess.
    let tla = crate::store::ToolName::new("tla").unwrap();
    std::fs::remove_file(l.shim(&tla)).unwrap();
    std::fs::remove_file(l.shim(&tla.alias().unwrap())).unwrap();
    assert_eq!(live_without_shim(&l, "ty"), None);
    // A name the store has never held stops at the store-directory check.
    assert!(!l.prefix.join("store").join("no-such-tool").exists());
    assert_eq!(live_without_shim(&l, "no-such-tool"), None);
}

/// A tool a program ships under ANOTHER name (`tla`, shipped by `ty`) answers for its
/// lost shim when the caller names the program — the reroute's question for `trustfmt`
/// — and only then: a standing shim, a build that does not ship it, or the wrong
/// program answers nothing.
#[cfg(unix)]
#[test]
fn a_lost_shim_of_a_shipped_tool_answers_for_the_program_that_ships_it() {
    let (l, d) = seed_ty_with_lost_shim("lost-shim-of", true);
    let found = live_without_shim_of(&l, "ty", "tla").expect("ty 3007 ships tla");
    assert_eq!(
        (found.build, found.binary),
        (3007, d.join("bin").join("tla"))
    );
    assert_eq!(live_without_shim_of(&l, "ty", "trustfmt"), None);
    assert_eq!(live_without_shim_of(&l, "trust", "tla"), None);
    assert_eq!(live_without_shim(&l, "tla"), None, "tla is not a program");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let (l, _) = seed_ty_with_lost_shim("lost-shim-of-standing", false);
    assert_eq!(
        live_without_shim_of(&l, "ty", "tla"),
        None,
        "tla's shim stands"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// AN AGENT PROGRAM'S LOST `bin/` SHIM IS NOT "NOTHING RUNS" WHERE ITS TWIN LEADS
/// (review, 2026-09-24). The `agents/` twin execs the store build, not `bin/claude`,
/// so with `agents/` ahead on PATH the managed copy still runs and `which` names the
/// twin; with no `agents/` on PATH nothing managed runs; and a foreign copy ahead of
/// `agents/` is what runs instead — the PATH order decides, both ways round.
#[cfg(unix)]
#[test]
fn which_names_the_agents_twin_that_still_runs_over_a_lost_bin_shim() {
    let l = temp_layout("lost-shim-agent");
    let d = l.build_dir("claude", 2_026_092_401);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let binary = d.join("bin").join("claude");
    std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::store::mark_build_ready(&d).unwrap();
    crate::activate::install_shims(
        &l,
        &d,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(&l, &d).unwrap();
    let claude = crate::store::ToolName::new("claude").unwrap();
    let twin = l.agent_shim(&claude);
    assert!(
        crate::platform::resolve_shim(&twin).is_some(),
        "fixture: the twin is laid"
    );
    std::fs::remove_file(l.shim(&claude)).unwrap();
    assert!(
        live_without_shim(&l, "claude").is_some(),
        "fixture: the lost-shim shape"
    );

    let foreign_dir = l.prefix.with_extension("foreign-claude");
    std::fs::create_dir_all(&foreign_dir).unwrap();
    let foreign = foreign_dir.join("claude");
    std::fs::write(&foreign, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755)).unwrap();
    let join = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();
    let agents = l.agents_dir();
    let which = |path: &std::ffi::OsStr| {
        which_line_in(&l, "claude", Some(path), crate::reroute::StubEnv::default()).unwrap()
    };

    let twin_runs = format!("claude → {} → {}", twin.display(), binary.display());
    for path in [join(&[&agents]), join(&[&agents, &foreign_dir])] {
        let line = which(&path);
        assert!(
            line.starts_with(&twin_runs)
                && line.contains("is missing")
                && line.contains("(fix: aterm pkg repair)"),
            "agents/ first: the twin runs the managed copy: {line}"
        );
        assert!(
            !line.contains("nothing runs") && !line.contains("runs instead"),
            "{line}"
        );
    }
    let line = which(&join(&[&foreign_dir, &agents]));
    assert!(
        line.starts_with(&format!("claude → {} — runs instead:", foreign.display())),
        "a foreign copy ahead of agents/ is what runs: {line}"
    );
    let line = which(&empty_path(&l));
    assert!(
        line.starts_with("claude → (nothing runs) — its shim"),
        "no agents/ and no foreign copy on PATH: nothing runs: {line}"
    );
    let _ = std::fs::remove_dir_all(&foreign_dir);
}

/// A LOST SHIM IS NOT AN UNINSTALLED PROGRAM (m3, 2026-09-23). With ty 3007 live and
/// `bin/ty` gone, `aterm pkg run ty` answered "ty is not installed (fix: aterm pkg
/// install ty)" — `install` answers up to date — and `aterm pkg which ty` called ty a
/// bundle exposing only `tla`. `run` now execs the store copy with a notice naming
/// `aterm pkg repair`, `which` says the shim is missing, and the named repair really
/// does put the shim back, after which the ordinary path answers with no notice.
#[cfg(unix)]
#[test]
fn run_and_which_answer_a_live_build_whose_own_shim_is_lost() {
    for all in [false, true] {
        let label = if all { "lost-shim-all" } else { "lost-shim-ty" };
        let (l, d) = seed_ty_with_lost_shim(label, all);
        let ty = crate::store::ToolName::new("ty").unwrap();
        let binary = d.join("bin").join("ty");

        assert_eq!(
            live_without_shim(&l, "ty"),
            Some(LostShim {
                build: 3007,
                shim: l.shim(&ty),
                binary: binary.clone(),
            }),
            "{label}"
        );
        match run_target(&l, "ty") {
            RunTarget::Exec {
                path,
                env,
                notice: Some(notice),
            } => {
                assert_eq!(path, binary, "{label}: run execs the store copy");
                assert_eq!(env, crate::shim_env::ShimEnv::NONE, "{label}");
                for want in [
                    "ty 3007 is installed",
                    "is missing",
                    "fix: aterm pkg repair",
                ] {
                    assert!(notice.contains(want), "{label}: {want:?} in {notice}");
                }
            }
            other => panic!("{label}: run must exec the live build, got {other:?}"),
        }

        let path = empty_path(&l);
        let line = which_line_in(&l, "ty", Some(&path), crate::reroute::StubEnv::default())
            .unwrap_or_else(|e| panic!("{label}: which refused: {e}"));
        for want in [
            "ty → (nothing runs) — its shim",
            "— managed 3007",
            "is missing",
            "fix: aterm pkg repair",
        ] {
            assert!(line.contains(want), "{label}: {want:?} in {line}");
        }
        for never in ["bundle", "not installed", "now:"] {
            assert!(!line.contains(never), "{label}: {never:?} in {line}");
        }

        // A foreign `ty` on PATH is what runs, and the line says so.
        // Outside the prefix: a copy inside it is never "foreign".
        let foreign_dir = l.prefix.with_extension("foreign");
        std::fs::create_dir_all(&foreign_dir).unwrap();
        let foreign = foreign_dir.join("ty");
        std::fs::write(&foreign, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755)).unwrap();
        let line = which_line_in(
            &l,
            "ty",
            Some(foreign_dir.as_os_str()),
            crate::reroute::StubEnv::default(),
        )
        .unwrap();
        assert!(
            line.starts_with(&format!("ty → {} — runs instead:", foreign.display()))
                && line.contains("is missing — managed 3007")
                && line.contains("(fix: aterm pkg repair)"),
            "{label}: {line}"
        );

        // The remedy named is the one that works: repair re-lays the shim, and then
        // run takes the ordinary path, with no notice.
        let (relaid, _, _, failed) = relay_shims(&l);
        assert_eq!((relaid, failed.len()), (1, 0), "{label}");
        assert_eq!(live_without_shim(&l, "ty"), None, "{label}: repaired");
        assert_eq!(
            run_target(&l, "ty"),
            RunTarget::Exec {
                path: binary.clone(),
                env: crate::shim_env::ShimEnv::NONE,
                notice: None,
            },
            "{label}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&foreign_dir);
    }
}

/// A LOST SHIM NEVER REVIVES WHAT THIS MACHINE TURNED OFF, and never answers for a
/// name the program does not own. Each control keeps the lost-shim shape of the test
/// above and changes one thing; every one must answer None and never exec.
#[cfg(unix)]
#[test]
fn a_lost_shim_answer_never_revives_or_borrows() {
    let ty = crate::store::ToolName::new("ty").unwrap();
    let not_run = |l: &crate::store::Layout, what: &str| {
        assert_eq!(live_without_shim(l, "ty"), None, "{what}");
        assert!(
            !matches!(run_target(l, "ty"), RunTarget::Exec { .. }),
            "{what}: run must not exec"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    };

    // A tombstone at bin/ty: the build was revoked, not lost.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-tomb-shim", false);
    crate::activate::install_tombstone_shim(&l, &ty).unwrap();
    not_run(&l, "tombstone shim");

    // A tombstone among its OTHER shims (the tla one), bin/ty absent.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-tomb-sibling", false);
    crate::activate::install_tombstone_shim(&l, &crate::store::ToolName::new("tla").unwrap())
        .unwrap();
    not_run(&l, "tombstone sibling");

    // A recorded `tombstoned:` row, and an `aborted:` one.
    for (label, state) in [
        ("lost-ctl-row-tomb", "tombstoned: pin yanked/below floor"),
        ("lost-ctl-row-abort", "aborted: group rolled back"),
    ] {
        let (l, _) = seed_ty_with_lost_shim(label, true);
        record_status(
            &l,
            "ty",
            crate::ProgramStatus {
                installed_build: Some(3007),
                state: state.into(),
                tree_root: String::new(),
            },
            "x".into(),
        );
        not_run(&l, label);
    }

    // Removed on purpose.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-removed", true);
    std::fs::write(l.removed(), "ty\n").unwrap();
    not_run(&l, "removed");

    // Dev-linked from a checkout, whose shim was then lost.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-linked", false);
    let checkout = l.prefix.join("checkout");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    let exe = checkout.join("bin").join("ty");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::linkmode::link(&l, "ty", &checkout, &[PathBuf::from("bin/ty")]).unwrap();
    std::fs::remove_file(l.shim(&ty)).unwrap();
    not_run(&l, "dev-linked");

    // A build that is not complete (its ready marker gone).
    let (l, d) = seed_ty_with_lost_shim("lost-ctl-incomplete", true);
    std::fs::remove_file(d.with_file_name("3007.ready")).unwrap();
    not_run(&l, "incomplete build");

    // A build that does not ship bin/ty.
    let (l, d) = seed_ty_with_lost_shim("lost-ctl-no-binary", false);
    std::fs::remove_file(d.join("bin").join("ty")).unwrap();
    crate::store::mark_build_ready(&d).unwrap();
    not_run(&l, "no bin/ty in the build");

    // A `current` link that names another program's build.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-foreign-current", true);
    let other = seed_ready_build(&l, "ay", 17);
    std::fs::remove_file(l.program_current("ty")).unwrap();
    std::os::unix::fs::symlink(&other, l.program_current("ty")).unwrap();
    not_run(&l, "current names another program");

    // Ownership: a trust bundle whose bin/ carries ty, while program ty is not here.
    let l = temp_layout("lost-ctl-bundle");
    let trust = seed_ready_build(&l, "trust", 1);
    let bundled = trust.join("bin").join("ty");
    std::fs::write(&bundled, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bundled, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::store::mark_build_ready(&trust).unwrap();
    crate::activate::install_shims(
        &l,
        &trust,
        &["trust".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(&l, &trust).unwrap();
    assert!(
        matches!(run_target(&l, "ty"), RunTarget::NotInstalled),
        "a bundle's ty is not program ty"
    );
    not_run(&l, "bundled same-named binary");

    // An alias name is never a program.
    let (l, _) = seed_ty_with_lost_shim("lost-ctl-alias", false);
    assert_eq!(live_without_shim(&l, "alab-ty"), None);
    let _ = std::fs::remove_dir_all(&l.prefix);

    // The unchanged path: the shim stands, so nothing is "lost".
    let (l, d) = seed_ty_with_lost_shim("lost-ctl-present", false);
    let (relaid, _, _, _) = relay_shims(&l);
    assert_eq!(relaid, 1);
    assert_eq!(live_without_shim(&l, "ty"), None);
    assert_eq!(
        run_target(&l, "ty"),
        RunTarget::Exec {
            path: d.join("bin").join("ty"),
            env: crate::shim_env::ShimEnv::NONE,
            notice: None,
        }
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// AN AGENT PROGRAM WITH A LOST SHIM IS NAMED, NOT RUN. Its run hands `claude update`
/// to the package manager only through a shim that resolves, so exec'ing the store
/// copy would let the vendor's updater run. `run` says it is installed and names the
/// repair; `which` offers no `now: aterm pkg run`.
#[cfg(unix)]
#[test]
fn a_lost_agent_shim_is_named_and_not_run() {
    let l = temp_layout("lost-agent");
    let d = seed_ready_build(&l, "claude", 20);
    activate_seeded(&l, "claude", &d);
    let claude = crate::store::ToolName::new("claude").unwrap();
    std::fs::remove_file(l.shim(&claude)).unwrap();

    assert_eq!(
        live_without_shim(&l, "claude").map(|lost| lost.build),
        Some(20)
    );
    match run_target(&l, "claude") {
        RunTarget::LostNotRun(line) => assert!(
            line.contains("claude 20 is installed")
                && line.contains("not run")
                && line.contains("fix: aterm pkg repair"),
            "{line}"
        ),
        other => panic!("an agent program's lost shim must not exec, got {other:?}"),
    }
    let path = empty_path(&l);
    let line = which_line_in(
        &l,
        "claude",
        Some(&path),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert!(
        line.contains("is missing") && !line.contains("now: aterm pkg run"),
        "{line}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A UNIT TEST OF A DOOR DOES NOT DRIVE LAUNCHD (2026-09-23). Every store-changing door
/// ends in the store heal, and a tracked test process (an agent's shell) tags every
/// file it writes, so every pass, install, seed or repair a unit test ran submitted one
/// real launchd job. Under test the heal is a scan-only stand-in unless the test opts
/// in ([`crate::provenance::test_bind`]): the door hands it exactly the store roots,
/// it answers what the real heal would have cleared, and the files keep their tag —
/// nothing ran that could clear it. Under an untracked test process there is nothing
/// to clear and the tag half is vacuous, and said so.
#[cfg(unix)]
#[test]
fn a_door_under_test_ends_in_the_scan_only_heal() {
    let l = temp_layout("door-scan-only-heal");
    let d19 = seed_ready_build(&l, "ay", 19);
    activate_seeded(&l, "ay", &d19);
    let _ = crate::provenance::test_bind::faked();
    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);
    let roots = crate::provenance::store_roots(&l);
    let faked = crate::provenance::test_bind::faked();
    assert_eq!(
        faked.len(),
        1,
        "one heal, at the end of the door: {faked:?}"
    );
    assert_eq!(faked[0].0, roots, "handed exactly the store roots");
    let now = crate::provenance::scan_roots(&roots, crate::provenance::PROVENANCE_XATTR);
    eprintln!(
        "this test process tagged {} store file(s)",
        faked[0].1.carriers.len()
    );
    assert_eq!(
        now.carriers, faked[0].1.carriers,
        "every file the stand-in found tagged still is: no job ran"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// EVERY DOOR THAT LEAVES THE STORE CHANGED CLEARS THE TAG, AND SAYS NOTHING ON STDOUT
/// (2026-09-23). The pass-end line that listed tagged builds on stdout for the window to
/// turn into a Warn row is gone; each of these doors ends in the silent heal instead, and
/// the heal's own words go only to the log.
#[test]
fn every_store_changing_door_ends_in_the_silent_heal() {
    fn body_of(src: &'static str, signature: &str) -> &'static str {
        let start = src.find(signature).expect("the function");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .expect("its closing brace");
        &src[start..end]
    }
    let src = include_str!("cli.rs");
    for door in [
        "fn run_update_pass(",
        "fn install_toolset(",
        "fn cmd_seed(",
        "fn cmd_install_with(",
        "fn rollback_in(",
        "fn cmd_update_one(",
    ] {
        assert!(
            body_of(src, door).contains("let _ = heal_store_provenance("),
            "{door} does not end in the heal"
        );
    }
    assert!(
        body_of(src, "fn repair_store(")
            .matches("repair_heal(layout)")
            .count()
            == 2,
        "repair heals whether or not anything is installed"
    );
    assert!(body_of(src, "fn repair_heal(").contains("crate::provenance::heal_store(layout)"));
    let heal = body_of(src, "fn heal_store_provenance(");
    assert!(
        !heal.contains("println!") && !heal.contains("eprintln!"),
        "the heal speaks only through the log: {heal}"
    );
}

/// REPAIR KEEPS WHAT THE BUILD EXPORTS (2026-09-23). `relay_shims` laid every shim
/// with no environment, so a `repair` on a machine with Claude Code installed rewrote
/// `bin/claude` without `export DISABLE_AUTOUPDATER=1` — and the vendor's own updater,
/// which installs copies this name never runs, came back on. Measured on m104 after a
/// 0.91.0 `aterm pkg repair`: the sidecar said `DISABLE_AUTOUPDATER=1`, the shim did
/// not. The re-lay now reads the build's sidecar.
#[cfg(unix)]
#[test]
fn repair_relays_a_shim_with_the_environment_its_build_declared() {
    let l = temp_layout("repair-shim-env");
    let d17 = seed_ready_build(&l, "ay", 17);
    activate_seeded(&l, "ay", &d17);
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()])
        .expect("an admitted env");
    crate::shim_env::write_sidecar(&d17, &env).expect("sidecar");
    let ay = crate::store::ToolName::new("ay").unwrap();
    assert_eq!(
        crate::platform::shim_env_of(&l.shim(&ay)),
        crate::shim_env::ShimEnv::NONE,
        "the seeded shim is laid plain — the drifted shape repair must heal"
    );

    let (relaid, _, _, failed) = relay_shims(&l);

    assert_eq!((relaid, failed.len()), (1, 0));
    assert_eq!(crate::platform::shim_env_of(&l.shim(&ay)), env);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// THE PASS HEALS WHAT REPAIR ONCE STRIPPED (audit 2026-09-23). An up-to-date program
/// is never laid again, so a `claude` shim a pre-2026-09-23 `repair` laid PLAIN over a
/// build whose sidecar declares `DISABLE_AUTOUPDATER=1` stayed plain — and the
/// `agents/` twin, which copies it, with it — until the next claude build. The pass's
/// alias reconcile now re-lays such a primary first, so the twin (and an ALab
/// program's `alab-` alias) carry the healed environment in the same pass; a shim
/// exporting something ELSE is put to exactly the declaration; a second pass writes
/// nothing; and a build with NO sidecar keeps whatever its shim exports (add-only).
#[cfg(unix)]
#[test]
fn the_pass_re_lays_a_primary_that_lost_its_builds_environment() {
    let l = temp_layout("pass-shim-env");
    let admit = |e: &str| crate::shim_env::ShimEnv::admit(&[e.to_string()]).unwrap();
    let env = admit("DISABLE_AUTOUPDATER=1");
    let dir = seed_ready_build(&l, "claude", 2_026_092_201);
    activate_seeded(&l, "claude", &dir); // PLAIN: the drifted shape
    crate::shim_env::write_sidecar(&dir, &env).expect("sidecar");
    let claude = crate::store::ToolName::new("claude").unwrap();
    let (shim, twin) = (l.shim(&claude), l.agent_shim(&claude));
    // An ALab program whose shim exports OTHER than its build declares, with its alias.
    let ay_dir = seed_ready_build(&l, "ay", 17);
    crate::activate::install_tools_env(
        &l,
        &ay_dir,
        &[crate::store::ToolName::new("ay").unwrap()],
        crate::activate::Aliases::Alab,
        &admit("AY_MODE=old"),
    )
    .unwrap();
    let ay_env = admit("AY_MODE=new");
    crate::shim_env::write_sidecar(&ay_dir, &ay_env).expect("sidecar");
    let ay = crate::store::ToolName::new("ay").unwrap();
    let alias = ay.alias().unwrap();
    assert_eq!(
        crate::platform::shim_env_of(&shim),
        crate::shim_env::ShimEnv::NONE
    );
    assert_eq!(
        crate::platform::shim_env_of(&twin),
        crate::shim_env::ShimEnv::NONE,
        "fixture: the twin mirrors the plain primary"
    );
    let index = index_of(&[("claude", None), ("ay", None)]);

    reconcile_aliases(&l, &index);

    assert_eq!(
        crate::platform::shim_env_of(&shim),
        env,
        "bin/claude healed"
    );
    assert_eq!(
        crate::platform::shim_env_of(&twin),
        env,
        "the twin mirrors it"
    );
    assert_eq!(
        crate::platform::resolve_shim(&shim),
        Some(dir.join("bin/claude")),
        "the same target"
    );
    assert_eq!(
        crate::platform::shim_env_of(&l.shim(&ay)),
        ay_env,
        "exactly the declaration, never the union"
    );
    assert_eq!(
        crate::platform::shim_env_of(&l.shim(&alias)),
        ay_env,
        "the alias mirrors the healed primary in the same pass"
    );
    let healed: Vec<(Vec<u8>, std::time::SystemTime)> = [&shim, &twin, &l.shim(&ay)]
        .iter()
        .map(|p| {
            (
                std::fs::read(p).unwrap(),
                std::fs::metadata(p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    reconcile_aliases(&l, &index);
    for (p, (bytes, mtime)) in [&shim, &twin, &l.shim(&ay)].iter().zip(&healed) {
        assert_eq!(&std::fs::read(p).unwrap(), bytes, "{}", p.display());
        assert_eq!(
            &std::fs::metadata(p).unwrap().modified().unwrap(),
            mtime,
            "a healed shim is never rewritten: {}",
            p.display()
        );
    }

    // ADD-ONLY: no sidecar ⇒ the shim's own exports stand.
    crate::shim_env::write_sidecar(&dir, &crate::shim_env::ShimEnv::NONE).unwrap();
    reconcile_aliases(&l, &index);
    assert_eq!(crate::platform::shim_env_of(&shim), env, "never stripped");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn repair_preserves_a_development_link_over_a_retained_store_build() {
    let l = temp_layout("repair-dev-link");
    let stored = seed_ready_build(&l, "ay", 17);
    activate_seeded(&l, "ay", &stored);
    let co = checkout("repair-dev-link", "ay");
    std::fs::set_permissions(
        co.join("target/release/ay"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    crate::link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    let ay = crate::store::ToolName::new("ay").unwrap();
    let target = crate::platform::resolve_shim(&l.shim(&ay)).unwrap();
    let marker = std::fs::read(l.link_marker("ay")).unwrap();
    assert!(target.starts_with(&co));

    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);

    assert_eq!(crate::platform::resolve_shim(&l.shim(&ay)), Some(target));
    assert_eq!(std::fs::read(l.link_marker("ay")).unwrap(), marker);
    assert!(crate::store::build_is_complete(&stored));
    assert!(crate::is_linked(&l, "ay"));
    assert!(crate::linkmode::live_dev_links(&l).0.contains("ay"));
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&co);
}

#[cfg(unix)]
#[test]
fn repair_refuses_malformed_development_link_intent_without_replacing_its_shim() {
    for kind in ["malformed", "oversized", "dangling", "linked-directory"] {
        let label = format!("repair-link-{kind}");
        let l = temp_layout(&label);
        let stored = seed_ready_build(&l, "ay", 17);
        activate_seeded(&l, "ay", &stored);
        let co = checkout(&label, "ay");
        crate::link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        let ay = crate::store::ToolName::new("ay").unwrap();
        let target = crate::platform::resolve_shim(&l.shim(&ay)).unwrap();
        let marker = l.link_marker("ay");
        match kind {
            "malformed" => std::fs::write(&marker, "not valid toml {{{\n").unwrap(),
            "oversized" => {
                std::fs::write(
                    &marker,
                    vec![b'x'; crate::linkmode::MAX_LINK_MARKER_BYTES + 1],
                )
                .unwrap();
            }
            "dangling" => {
                std::fs::remove_file(&marker).unwrap();
                std::os::unix::fs::symlink(l.prefix.join("absent-marker"), &marker).unwrap();
            }
            "linked-directory" => {
                let kept = l.prefix.join("links-kept");
                std::fs::rename(l.links_dir(), &kept).unwrap();
                std::os::unix::fs::symlink(&kept, l.links_dir()).unwrap();
            }
            _ => unreachable!(),
        }

        assert_eq!(repair_store(&l, &[]), ExitCode::from(1), "{kind}");
        assert_eq!(
            crate::platform::resolve_shim(&l.shim(&ay)),
            Some(target),
            "{kind}: a malformed marker never authorizes replacing the checkout"
        );
        assert!(std::fs::symlink_metadata(&marker).is_ok(), "{kind}");
        assert!(crate::store::build_is_complete(&stored));
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&co);
    }
}

/// REPAIR RESPECTS A ROLLBACK (audit K2, 2026-09-12). `rollback` keeps the build it
/// rolled off until gc; repair must re-lay the build `current` selects, not the highest
/// one on disk — or it silently undoes the rollback and mints the very
/// `LinkShimMismatch` doctor then reports.
#[test]
fn repair_respects_rollback() {
    let l = temp_layout("repair-rollback");
    let d18 = seed_ready_build(&l, "ay", 18);
    let d19 = seed_ready_build(&l, "ay", 19);
    activate_seeded(&l, "ay", &d19);
    // The rollback: shims and both links back on 18, 19 retained on disk.
    activate_seeded(&l, "ay", &d18);
    let ay = crate::store::ToolName::new("ay").unwrap();

    let _ = relay_shims(&l);

    let target = crate::platform::resolve_shim(&l.shim(&ay)).expect("a live shim");
    assert!(
        target.starts_with(&d18),
        "repair re-pointed the shim off the rollback target: {}",
        target.display()
    );
    assert!(
        crate::gc::live_builds(&l).diverged().is_empty(),
        "{:?}",
        crate::gc::live_builds(&l).diverged()
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A COMPLETE trust build in the shape bundles 8571/8589/8590/8595 ship: `bin/rustc` a
/// separate file beside `bin/trustc` whose bytes differ (the ad-hoc signature bakes each
/// file's name in), a tippy that refuses that pair, and a `lib/` with a driver dylib —
/// with its ready marker, `store/trust/current` pointing at it, and PLAIN shims for
/// `trustc`, `targo`, `tippy` and `alab-tippy` written the way a client before exec roots
/// laid them. Returns the build dir.
fn seed_affected_trust(layout: &crate::store::Layout, build: u64) -> PathBuf {
    let dir = layout.build_dir("trust", build);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib").join("librustc_driver-5cfd.dylib"),
        b"driver",
    )
    .unwrap();
    for tool in ["targo", "tippy", "targo-tippy", "tippy-driver"] {
        std::fs::write(bin.join(tool), format!("{tool} {build}")).unwrap();
    }
    std::fs::write(bin.join("trustc"), b"frontend / signature: trustc").unwrap();
    std::fs::write(bin.join("rustc"), b"frontend / signature: rustc_").unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::atomic_symlink(&dir, &layout.program_current("trust")).unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
    layout.ensure_dir(&layout.bin_dir()).unwrap();
    for (name, tool) in [
        ("trustc", "trustc"),
        ("targo", "targo"),
        ("tippy", "tippy"),
        ("alab-tippy", "tippy"),
    ] {
        let shim = layout.bin_dir().join(name);
        std::fs::write(
            &shim,
            crate::platform::sh_shim_content_env(&bin.join(tool), &crate::shim_env::ShimEnv::NONE),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// Whether the shim `bin/<name>` carries an exec-root guard line.
fn routes(layout: &crate::store::Layout, name: &str) -> bool {
    std::fs::read_to_string(layout.bin_dir().join(name))
        .unwrap()
        .lines()
        .any(|l| l.starts_with("[ ! -h "))
}

/// THE PASS HEALS AN UP-TO-DATE AFFECTED MACHINE WITHOUT AN INSTALL. A machine already
/// on trust 8595 with the plain shims an older client laid is short-circuited by the
/// apply before any shim is written; [`reconcile_exec_roots`] alone — no
/// `install_tools` — lays `compat/trust/8595` and routes every trust shim, the `alab-`
/// alias included, and answers `true`. A second call writes nothing. And the verbs that
/// must heal call it where the design puts them — each call pinned AFTER its anchor and
/// BEFORE the first return or branch that would skip it on the lane that needs it: the
/// update pass after its gc and before `if failures == 0` (a pass with one failed
/// program still heals), the default-set lane after its alias reconcile, `seed` beside
/// the rustup seam and before its closing verdict, a trust rollback after its seam, and
/// `repair` at `Deep` through [`repair_store`], before its "done". This pins the order
/// of the source text, not a run of those verbs, which write the account's real
/// `defaults` or rc wiring from inside a test process; a reviewer's mutations that moved
/// the seed call below the seedless return, or the pass's inside `if failures == 0`,
/// passed the anchor-only form of this test (2026-09-15).
#[test]
fn the_pass_reconcile_routes_an_up_to_date_affected_install() {
    let l = temp_layout("exec-root-pass");
    seed_affected_trust(&l, 8595);
    let shims = ["trustc", "targo", "tippy", "alab-tippy"];
    assert!(shims.iter().all(|s| !routes(&l, s)), "precondition: plain");

    assert!(reconcile_exec_roots(&l, crate::seam::Depth::Shallow));
    let root = crate::compat::root_dir(&l, 8595);
    assert!(root.join("bin").join("rustc").is_file());
    for s in shims {
        assert!(routes(&l, s), "{s} routes");
    }
    let modified = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();
    let before: Vec<_> = shims
        .iter()
        .map(|s| modified(l.bin_dir().join(s)))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(reconcile_exec_roots(&l, crate::seam::Depth::Deep));
    let after: Vec<_> = shims
        .iter()
        .map(|s| modified(l.bin_dir().join(s)))
        .collect();
    assert_eq!(before, after, "a second reconcile re-lays nothing");

    let production = production_half(include_str!("cli.rs"));
    let body = |name: &str| {
        let start = production
            .find(&format!("\nfn {name}("))
            .unwrap_or_else(|| panic!("fn {name}"));
        let end = production[start + 1..]
            .find("\nfn ")
            .map_or(production.len(), |i| start + 1 + i);
        &production[start..end]
    };
    // `call` appears after `anchor` and before the first `until` after `anchor`.
    let between = |name: &str, anchor: &str, call: &str, until: &str| {
        let b = body(name);
        let a = b.find(anchor).unwrap_or_else(|| panic!("{name}: {anchor}"));
        let c = b[a..]
            .find(call)
            .unwrap_or_else(|| panic!("{name} calls `{call}` after `{anchor}`"));
        let u = b[a..]
            .find(until)
            .unwrap_or_else(|| panic!("{name}: `{until}` after `{anchor}`"));
        assert!(c < u, "{name} calls `{call}` before `{until}`");
    };
    const SHALLOW: &str = "reconcile_exec_roots(&layout, crate::seam::Depth::Shallow)";
    between(
        "run_update_pass",
        "print_gc_abstentions(\"update\"",
        "reconcile_exec_roots(layout, crate::seam::Depth::Shallow)",
        "if failures == 0 {",
    );
    between(
        "install_default_set_inner",
        "reconcile_aliases(layout, index);",
        "reconcile_exec_roots(layout, crate::seam::Depth::Shallow)",
        "reconcile_shadowed(",
    );
    between(
        "cmd_seed",
        "reassert_rustup_seam(&layout);",
        SHALLOW,
        "report_seedless_posture(&layout);",
    );
    between(
        "rollback_in",
        "reassert_rustup_seam(layout);",
        "reconcile_exec_roots(layout, crate::seam::Depth::Shallow)",
        "ExitCode::SUCCESS",
    );
    between(
        "repair_store",
        "relay_shims(layout)",
        "reconcile_exec_roots(layout, crate::seam::Depth::Deep)",
        "repair: done",
    );
    // THE LANES A PERSON TYPES (2026-09-17). Every whole-set pass above has re-asserted
    // the rustup seam and reconciled the exec roots since those calls were written;
    // `aterm pkg install <p>` and `aterm pkg update <p>` — which end in
    // `cmd_install_with`, or in `cmd_update_one`'s coherence-group branch — never did.
    // That is the asymmetry that let `aterm pkg update trust` answer "up to date" for
    // two weeks on m3 without once looking at the `~/.rustup/toolchains/trust` entry
    // that decided what the machine actually compiled with. The seam comes first in
    // both, matching every other lane: a root laid over a seam that was just re-pointed
    // is the order `cmd_seed` and `cmd_rollback` already use.
    for lane in ["cmd_install_with", "cmd_update_one"] {
        let b = body(lane);
        let seam = b
            .find("reassert_rustup_seam(")
            .unwrap_or_else(|| panic!("{lane} must re-assert the rustup seam"));
        let roots = b
            .find("reconcile_exec_roots(")
            .unwrap_or_else(|| panic!("{lane} must reconcile the exec roots"));
        assert!(
            seam < roots,
            "{lane}: the seam is re-asserted before the roots"
        );
    }
    assert!(body("run_repair").contains("let leftovers = repair_copied_commands();"));
    assert!(body("run_repair").contains("repair_store(&layout, &leftovers)"));
    assert_eq!(
        body("cmd_uninstall_all")
            .matches("remove_exec_roots(&layout);")
            .count(),
        2,
        "both branches of uninstall --all remove compat/"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// REPAIR ROUTES, AND SAYS SO IN ITS EXIT CODE WHEN IT CANNOT. Over an affected build
/// with plain shims, [`repair_store`] re-lays them through the root it lays and exits
/// 0; with the root unlayable (an unreadable directory in the build's `lib/`, standing
/// in for the EXDEV of a store on another volume) every shim is still re-laid — plain,
/// running the store path as before — and repair exits 1, because doctor's fix for a
/// PATH tippy that cannot run is exactly this verb.
#[test]
fn repair_routes_the_trust_shims_and_exits_1_when_the_root_cannot_be_laid() {
    let l = temp_layout("exec-root-repair");
    seed_affected_trust(&l, 8595);
    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);
    for s in ["trustc", "targo", "tippy", "alab-tippy"] {
        assert!(routes(&l, s), "{s} routes after repair");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);

    if crate::platform::our_uid() == 0 {
        return; // root reads a 0000 directory; the failure cannot be staged
    }
    let l = temp_layout("exec-root-repair-fail");
    let dir = seed_affected_trust(&l, 8595);
    let locked = dir.join("lib").join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let code = repair_store(&l, &[]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, ExitCode::from(1));
    for s in ["trustc", "targo", "tippy", "alab-tippy"] {
        assert!(!routes(&l, s), "{s} stays plain");
        assert!(
            crate::platform::resolve_shim(&l.bin_dir().join(s)).is_some(),
            "{s} is still a live shim"
        );
    }
    assert!(std::fs::symlink_metadata(crate::compat::root_dir(&l, 8595)).is_err());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A ROW MUST NOT OUTLIVE ITS PROGRAM.
///
/// `active_builds` reads shims, so doctor went quiet after an uninstall — but Settings ▸
/// Packages reads the RECORD, and listed a deleted program as `active` at its last build
/// forever. The stale-row reconciler cannot fix this and correctly declines to: the
/// signed index still names the program, it is this MACHINE that no longer carries it.
#[test]
fn uninstalling_retires_the_program_row() {
    let layout = temp_layout("retire-row");
    make_live(&layout, "ay", 18);
    make_live(&layout, "clean", 7);
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: "active".into(),
            tree_root: "deadbeef".into(),
        },
        "up to date".into(),
    );
    record_status(
        &layout,
        "clean",
        crate::ProgramStatus {
            installed_build: Some(7),
            state: "active".into(),
            tree_root: "cafe".into(),
        },
        "up to date".into(),
    );
    assert!(
        crate::active_builds(&layout).contains_key("ay"),
        "precondition: ay is genuinely live"
    );

    // THE REAL PATH the CLI arm runs — not the primitive it happens to call.
    uninstall_and_retire(&layout, "ay").expect("uninstall succeeds");

    assert!(
        !crate::active_builds(&layout).contains_key("ay"),
        "the program really came off"
    );
    assert!(
        removed_programs(&layout).contains("ay"),
        "and the seed lane is told not to put it back"
    );
    let s = crate::status::read(&layout).expect("record present");
    assert!(
        !s.programs.contains_key("ay"),
        "the removed program's row is retired, not left reading `active`"
    );
    assert!(
        s.programs.contains_key("clean"),
        "and retiring one program does not disturb the others"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A HELD-UPDATE RECORD ENDS WITH THE LAST BUILD IT MOVES OFF (review of 1686f3bfa,
/// 2026-09-26): doctor reports an outlived record and changes nothing, so the doors that
/// write end it — the uninstall that took its builds, and `repair`, the door doctor
/// names for one a store emptied by hand left. The NEGATIVE CONTROL is a record one of
/// whose builds still stands: kept by both, as the wait it still is.
#[test]
fn uninstall_and_repair_end_a_held_update_record_that_outlived_its_builds() {
    let layout = temp_layout("held-outlived-doors");
    make_live(&layout, "ay", 18);
    make_live(&layout, "clean", 7);
    let note = |from: &[(&str, u64)]| {
        let from: std::collections::BTreeMap<String, u64> =
            from.iter().map(|(p, b)| ((*p).to_string(), *b)).collect();
        let to = from.keys().map(|p| (p.clone(), 99u64)).collect();
        assert!(crate::quiet::note_deferred(
            &layout,
            "rustc",
            1_790_000_000,
            1_790_000_000,
            &crate::quiet::Busy::Unknown {
                why: String::from("the process table could not be read"),
            },
            &crate::quiet::Moves {
                from,
                to,
                assets: std::collections::BTreeMap::new(),
            },
        ));
    };
    let held = || crate::quiet::read(&layout).is_some();

    // One build it moves off still stands: kept by the uninstall and by repair.
    note(&[("ay", 18), ("clean", 7)]);
    uninstall_and_retire(&layout, "ay").expect("uninstall succeeds");
    assert!(held(), "clean 7 still stands: the record is a wait");
    let _ = repair_store(&layout, &[]);
    assert!(held(), "and repair keeps it");

    // The uninstall that takes its last build ends it.
    uninstall_and_retire(&layout, "clean").expect("uninstall succeeds");
    assert!(
        !held(),
        "nothing it moves off is left: the record ended with them"
    );

    // A record a store emptied by hand left: repair, the door doctor names, ends it.
    note(&[("clean", 7)]);
    assert!(matches!(
        crate::quiet::held(&layout),
        crate::quiet::Held::Outlived(_)
    ));
    let _ = repair_store(&layout, &[]);
    assert!(!held(), "repair ended it");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// ROW DELETION IS A NEW CAPABILITY, so the names it accepts are gated.
///
/// `ops::uninstall`'s shape rule rejects only empty/`.`/`..`/separators/NUL, so before
/// the gate `atpkg uninstall '*toolset*'` returned Ok and DELETED a bookkeeping row that
/// has its own owner and its own reaper, and `atpkg uninstall --help` appended a flag to
/// the removed-markers file — which would then suppress a program named `--help` forever,
/// a fittingly absurd end for the bug that started this.
#[test]
fn retirement_refuses_names_that_are_not_programs() {
    let layout = temp_layout("retire-gate");
    record_status(
        &layout,
        "*toolset*",
        crate::ProgramStatus {
            installed_build: None,
            state: "unavailable: no build for this architecture".into(),
            tree_root: String::new(),
        },
        "no build published".into(),
    );

    assert!(
        uninstall_and_retire(&layout, "*toolset*").is_err(),
        "a sentinel bookkeeping row is not a program and is not this verb's to retire"
    );
    assert!(
        crate::status::read(&layout)
            .expect("record present")
            .programs
            .contains_key("*toolset*"),
        "…and it is still there"
    );
    assert!(
        uninstall_and_retire(&layout, "--help").is_err(),
        "a flag is not a program name here either"
    );
    assert!(
        !removed_programs(&layout).contains("--help"),
        "no removed-marker is minted for a flag"
    );
    // The removal intent is recorded BEFORE the delete, so the shape rule `ops::uninstall`
    // enforces must refuse first too — a path-shaped name is not a program to remember.
    assert!(uninstall_and_retire(&layout, "a/b").is_err());
    assert!(
        removed_programs(&layout).is_empty(),
        "no removed-marker is minted for a name the delete would refuse"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE REMOVAL INTENT LANDS BEFORE THE DELETE.
///
/// `ops::uninstall` takes the shims off first and then spends seconds in `remove_dir_all`
/// on the store tree (3.4 GB for trust). The removed marker and the adoption clear used to
/// land only after that returned, so a kill inside the delete left the program absent to
/// `active_builds` and recorded nowhere, and the next six-hourly pass reinstalled it on
/// the machine whose owner had just removed it. A kill is not reproducible in a test; a
/// subtree the delete cannot empty stops `remove_dir_all` at the same point and leaves the
/// same machine state.
#[cfg(unix)]
#[test]
fn a_delete_cut_short_still_leaves_the_removal_recorded() {
    let layout = temp_layout("uninstall-cut-short");
    make_live(&layout, "ay", 18);
    std::fs::write(layout.adopted(), b"").unwrap();
    let locked = layout.build_dir("ay", 18).join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("payload"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::write(locked.join("probe"), b"x").is_ok() {
        // Privileged (root in a container): permissions cannot stop the delete here.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&layout.prefix);
        return;
    }

    let code = uninstall_one(&layout, "ay");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(1)),
        "precondition: the delete really stopped part-way"
    );
    assert!(
        !crate::active_builds(&layout).contains_key("ay") && program_present(&layout, "ay"),
        "precondition: shims gone, tree half there — what a kill mid-delete leaves"
    );
    assert!(
        layout.removed_programs().contains("ay"),
        "the unattended lanes are told the removal was deliberate"
    );
    assert!(
        !adopted(&layout),
        "and set-completion was opted out of before the delete"
    );
    assert!(
        std::fs::read_dir(&layout.prefix)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("removed.tmp-")),
        "the marker is renamed into place, leaving no temp file behind"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// AN OFFLINE FIRST RUN MUST STILL LEAVE A WITNESS.
///
/// Suppressing the environmental failures' per-program rows routes their reason to the
/// outcome sentence and NOWHERE else — so the sentence has to survive a virgin store.
/// While `record_outcome` early-returned when no record existed, a first-run offline
/// install recorded nothing at all: exactly the failed-seed machine that gets pointed
/// at `pkg doctor`, and the one case doctor's fallback to this sentence exists for.
#[test]
fn the_outcome_sentence_creates_the_record_it_needs() {
    let layout = temp_layout("virgin-outcome");
    assert!(
        crate::status::read(&layout).is_none(),
        "precondition: this store has never been written to"
    );
    record_outcome(
        &layout,
        String::from("install ay: no signature-valid index"),
    );
    let s = crate::status::read(&layout).expect("a first-run failure still leaves a witness");
    assert_eq!(s.outcome, "install ay: no signature-valid index");
    assert!(
        s.programs.is_empty(),
        "the witness records what happened without blaming a program for the network"
    );
    assert_eq!(s.schema, 1, "a created record is a well-formed one");
}

/// The record is a DERIVED memo; the signed index is the authority on what exists.
///
/// Pins the four conditions together, because each one alone is a different bug: prune
/// a sentinel and the bookkeeping rows lose their reapers; prune a live-but-dropped
/// program and `atpkg verify` fails closed forever on an erased attestation.
#[test]
fn the_record_reconciles_against_the_signed_index() {
    let row = |state: &str| crate::ProgramStatus {
        installed_build: None,
        state: state.into(),
        tree_root: String::new(),
    };
    let recorded = std::collections::BTreeMap::from([
        (
            "--help".to_string(),
            row("error: --help is not named in the signed index"),
        ),
        (
            "ayy".to_string(),
            row("error: ayy is not named in the signed index"),
        ),
        ("ay".to_string(), row("active")),
        (
            "*toolset*".to_string(),
            row("unavailable: no build for this Mac"),
        ),
        ("retired".to_string(), row("active")),
    ]);
    let live = std::collections::BTreeMap::from([
        ("ay".to_string(), 18_u64),
        ("retired".to_string(), 5_u64),
    ]);
    let pruned = prunable_status_rows(&recorded, &live, &|n| n == "ay");

    assert_eq!(
        pruned,
        vec!["--help".to_string(), "ayy".to_string()],
        "only the rows naming nothing the index knows and nothing the store carries"
    );
    assert!(
        !pruned.contains(&"*toolset*".to_string()),
        "sentinel bookkeeping rows have their own owners and their own reapers"
    );
    assert!(
        !pruned.contains(&"retired".to_string()),
        "a program dropped upstream while still LIVE keeps its row — deleting it would \
         erase the attestation `atpkg verify` fails closed without"
    );
}

/// The silent update loop calls `record_status` once PER program; recording one
/// must not wipe the others' last-known state (regression for the clobber bug).
#[test]
fn record_status_merges_programs_rather_than_clobbering() {
    let layout = temp_layout("merge");
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: "active".into(),
            tree_root: String::new(),
        },
        "up to date".into(),
    );
    record_status(
        &layout,
        "trust",
        crate::ProgramStatus {
            installed_build: Some(4821),
            state: "active".into(),
            tree_root: String::new(),
        },
        "staged 4821".into(),
    );
    let s = crate::status::read(&layout).expect("status present");
    assert!(
        s.programs.contains_key("ay"),
        "first program preserved across the second write"
    );
    assert!(s.programs.contains_key("trust"), "second program recorded");
    assert_eq!(s.programs["ay"].installed_build, Some(18));
    assert_eq!(s.programs["trust"].installed_build, Some(4821));
    assert_eq!(
        s.outcome, "staged 4821",
        "aggregate outcome reflects the latest pass"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// AN UNREADABLE RECORD IS HEALED, NEVER REFUSED FOR EVER (Phase 3, 2026-09-22). The
/// writers used to decline an unreadable `status.toml` so as not to destroy it — and so
/// nothing could ever stamp `last_success_at` again, the window's launch pass was due
/// at every launch, and "never checked" stood until a person moved the file. Now the
/// first rewriting writer keeps the unreadable bytes as `status.toml.corrupt-<unix>`
/// (nothing an operator could salvage is overwritten) and records onto one rebuilt
/// from the store, so the pass that follows stamps as usual.
#[test]
fn status_writers_heal_an_unreadable_record() {
    let layout = temp_layout("unreadable-status");
    record_status(
        &layout,
        "trust",
        crate::ProgramStatus {
            installed_build: Some(6808),
            state: "active".into(),
            tree_root: "abc123".into(),
        },
        "up to date".into(),
    );
    record_pass_end(&layout, now_unix(), true);
    let good = std::fs::read_to_string(layout.status()).unwrap();
    let corrupt = good + "this is not valid toml {{{\n";
    std::fs::write(layout.status(), &corrupt).unwrap();

    // The failed-pass row: the exact write the 6-hour tick makes when it is offline.
    record_status(
        &layout,
        "*index*",
        crate::ProgramStatus {
            installed_build: None,
            state: "error: index unreachable".into(),
            tree_root: String::new(),
        },
        "update failed: index unreachable".into(),
    );
    let healed = crate::status::read_checked(&layout)
        .expect("the record reads again")
        .expect("and exists");
    assert_eq!(healed.programs["*index*"].state, "error: index unreachable");
    assert_eq!(healed.outcome, "update failed: index unreachable");
    let kept: Vec<std::path::PathBuf> = std::fs::read_dir(&layout.prefix)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("status.toml.corrupt-"))
        })
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the unreadable bytes are kept aside: {kept:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&kept[0]).unwrap(),
        corrupt,
        "byte for byte"
    );
    // …and the next success stamps, as it could not before.
    record_pass_end(&layout, now_unix(), true);
    assert!(!crate::status::never_checked(&layout));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE `[machine]` SETTINGS DO NOT RIDE THE PASS (Phase 3, 2026-09-22; replacing the
/// 2026-09-14/15 laws that ran them at the top of every pass, above the lock). Their
/// $HOME walk (up to 60,000 directories / 3 s) and `defaults` cost every pass and every
/// `claude update` child; they now run from their own verb, which the window and the
/// session call on their schedule, and at a pass's DISPATCH EDGE only when the table
/// changed (`tests/machine_edge.rs` drives that, and that a pass under a private state
/// root carries none). The verbs that may carry an edit are the long passes, never a
/// quick mutator.
#[test]
fn only_the_long_passes_carry_a_changed_machine_table() {
    for verb in ["install", "seed", "update"] {
        assert!(verb_applies_machine_settings(verb), "{verb}");
        assert!(verb_mutates_store(verb), "{verb} also takes the lock");
    }
    for verb in [
        "pin", "unpin", "gc", "link", "unlink", "refresh", "repair", "machine",
    ] {
        assert!(!verb_applies_machine_settings(verb), "{verb}");
    }
}

/// WHAT AN APPLY MUST RETRY: a live build it skipped and a migration that failed. A
/// directory that was already excluded, migrated, or declined for a reason no retry
/// changes (not beside a Cargo.toml) leaves nothing to retry.
#[test]
fn an_apply_is_finished_only_when_nothing_is_left_to_retry() {
    use crate::noindex::Applied;
    let p = std::path::PathBuf::from("/h/repo/target");
    let excluded = Applied::AlreadyExcluded(p.clone());
    let declined = Applied::Skipped {
        path: p.clone(),
        reason: "not beside a Cargo.toml — its pointer is an env var".into(),
    };
    assert!(spotlight_apply_finished(&[excluded.clone(), declined]));
    assert!(spotlight_apply_finished(&[]));
    let live = Applied::Skipped {
        path: p.clone(),
        reason: format!(
            "{}{} — retried next pass",
            crate::noindex::HELD_BY_BUILD,
            p.join("debug/.cargo-lock").display()
        ),
    };
    assert!(live.held_by_build(), "{live:?}");
    assert!(!spotlight_apply_finished(&[excluded.clone(), live]));
    let failed = Applied::Failed {
        path: p,
        reason: "rename refused".into(),
    };
    assert!(!spotlight_apply_finished(&[excluded, failed]));
}

/// A pass that moved nothing and failed nothing reports from the walk it made: its scan's
/// counts, and nothing left to migrate — every exposed dir it left was a refusal a dry run
/// refuses identically. A pass that moved something walks again, because the pre-apply
/// scan no longer describes the disk.
#[cfg(target_os = "macos")]
#[test]
fn the_census_after_an_apply_is_the_scan_unless_something_moved() {
    use crate::noindex::{Applied, Budget, CACHEDIR_TAG_SIGNATURE, ConfigNote, DOCTOR_DEPTH};
    let home = crate::testdir::dir("atpkg-census-after-apply-");
    // One repo whose target dir is exposed by name.
    let repo = home.join("repo");
    let target = repo.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        format!("{CACHEDIR_TAG_SIGNATURE}\n# cargo\n"),
    )
    .unwrap();
    let scan = crate::noindex::scan(&home, DOCTOR_DEPTH, &Budget::DOCTOR);
    assert_eq!(
        scan.counts(),
        (1, 0),
        "precondition: one exposed target dir"
    );
    // Nothing moved (say, a live build refused it): the scan is the answer, and
    // nothing is left for an apply to do. The pass's rows about its own links are no
    // exposed dir: a tree a build held while it was to be moved back, and a link the
    // pass did not lay ([`crate::noindex::apply_under_scan`]).
    let refused = vec![
        Applied::Skipped {
            path: target.clone(),
            reason: "a build holds it".into(),
        },
        Applied::Skipped {
            path: home.join("old/target"),
            reason: "a build holds /h/old/target.noindex/debug/.cargo-lock — retried next \
                     pass"
                .into(),
        },
        Applied::Skipped {
            path: home.join("hand/target"),
            reason: "git status shows `?? target`, and this is not the link the pass laid".into(),
        },
    ];
    let ledger = crate::noindex::Ledger::at(home.join(".noindex.ledger"));
    let census = spotlight_after_apply(&home, &scan, &refused, &ledger);
    assert_eq!(
        (
            census.exposed,
            census.hidden,
            census.would_migrate,
            census.complete
        ),
        (1, 0, 0, true)
    );
    assert_eq!(
        census.open,
        OpenSplit {
            busy: 1,
            ..OpenSplit::default()
        },
        "the dir a build held is counted as busy, not as free-standing or refused"
    );
    // Something moved: the disk changed under the scan, so the census walks again
    // and reports what is there now.
    let hidden = repo.join("target.noindex");
    std::fs::rename(&target, &hidden).unwrap();
    let moved = vec![Applied::Migrated {
        from: target.clone(),
        to: hidden.clone(),
        config: ConfigNote::Untouched("test".into()),
        discarded: false,
    }];
    let census = spotlight_after_apply(&home, &scan, &moved, &ledger);
    assert_eq!(
        (census.exposed, census.hidden, census.would_migrate),
        (0, 1, 0),
        "after the move the record must say hidden, from a fresh walk"
    );
}

/// `atpkg machine`'s Spotlight sentence says WHY each exposed dir stays open, from the
/// dry run's own reasons — never a guess between causes it can tell apart — and, with the
/// switch off, that the switch is the reason.
#[test]
fn the_machine_spotlight_line_names_why_each_dir_stays_open() {
    use crate::noindex::Applied;
    let p = std::path::PathBuf::from;
    let mut open = OpenSplit::default();
    for o in [
        Applied::Skipped {
            path: p("/h/free/target"),
            reason: "not beside a Cargo.toml — its pointer is an env var".into(),
        },
        Applied::Skipped {
            path: p("/h/busy/target"),
            reason: "a build holds /h/busy/target/debug/.cargo-lock — retried next pass".into(),
        },
        Applied::Failed {
            path: p("/h/taken/target"),
            reason: "/h/taken/target.noindex already exists".into(),
        },
        Applied::AlreadyExcluded(p("/h/done/target")),
    ] {
        open.count(&o);
    }
    assert_eq!(
        open,
        OpenSplit {
            free_standing: 1,
            busy: 1,
            refused: 1
        }
    );
    let census = SpotlightCensus {
        exposed: 5,
        hidden: 2,
        would_migrate: 2,
        open,
        complete: true,
    };
    assert_eq!(
        spotlight_census_line("/h", &census, true),
        "5 cargo target dir(s) under /h open to Spotlight: 2 an apply hides, 1 \
         free-standing (`aterm pkg noindex apply <dir>` by name), 1 in a running build \
         (the next apply retries it), 1 refused (`aterm pkg noindex apply <dir>` says \
         why); 2 hidden"
    );
    // Only the non-zero parts.
    let only = SpotlightCensus {
        exposed: 2,
        hidden: 0,
        would_migrate: 2,
        open: OpenSplit::default(),
        complete: false,
    };
    assert_eq!(
        spotlight_census_line("/h", &only, true),
        "2 cargo target dir(s) under /h open to Spotlight: 2 an apply hides; 0 hidden — \
         at least; the scan hit its budget"
    );
    // Switch off: the switch is why they are open, whatever an apply could do.
    let off = spotlight_census_line("/h", &census, false);
    assert_eq!(
        off,
        "5 cargo target dir(s) under /h open to Spotlight, 2 hidden — [machine] \
         spotlight_noindex = false leaves them"
    );
    assert!(!off.contains("free-standing"), "{off}");
}

/// `atpkg machine apply` with nothing changed names the one cause it saw.
#[test]
fn machine_apply_says_why_nothing_changed() {
    let say = |m: MachineApply| m.nothing_changed();
    assert_eq!(
        say(MachineApply::default()),
        "nothing changed — already applied"
    );
    assert_eq!(
        say(MachineApply {
            both_off: true,
            ..MachineApply::default()
        }),
        "nothing changed — [machine] switches both settings off"
    );
    let busy = |n| OpenSplit {
        busy: n,
        ..OpenSplit::default()
    };
    assert_eq!(
        say(MachineApply {
            open: busy(2),
            ..MachineApply::default()
        }),
        "nothing changed — 2 cargo target dir(s) stay open: 2 in a running build (the next \
         apply retries it)"
    );
    // Free-standing dirs and a walk cut at its budget are not "already applied".
    assert_eq!(
        say(MachineApply {
            open: OpenSplit {
                free_standing: 1,
                ..OpenSplit::default()
            },
            walk_cut: true,
            ..MachineApply::default()
        }),
        "nothing changed — 1 cargo target dir(s) stay open: 1 free-standing (`aterm pkg \
         noindex apply <dir>` by name); the walk stopped at its budget"
    );
    assert_eq!(
        say(MachineApply {
            walk_cut: true,
            ..MachineApply::default()
        }),
        "nothing changed — the walk stopped at its budget; `aterm pkg noindex apply --all` \
         walks further"
    );
    assert_eq!(
        say(MachineApply {
            failed: true,
            open: busy(1),
            ..MachineApply::default()
        }),
        "nothing changed — a change did not land"
    );
    assert_eq!(
        say(MachineApply {
            no_home: true,
            ..MachineApply::default()
        }),
        "nothing changed — there is no home directory to walk for build output"
    );
}

/// `atpkg relocate`'s first line reads the whole report: a rewrite with nothing vendored
/// is not "already portable", and an unresolved reference leaves that line out.
#[test]
fn relocate_says_already_portable_only_when_nothing_was_found() {
    use crate::relocate::Report;
    let rewrote = Report {
        rewritten: 3,
        ..Report::default()
    };
    assert_eq!(
        relocate_headline(&rewrote).as_deref(),
        Some("atpkg relocate: rewrote 3 object(s); nothing to vendor")
    );
    let unresolved = Report {
        unresolved: vec!["bin/x: source not found to vendor".into()],
        ..Report::default()
    };
    assert_eq!(relocate_headline(&unresolved), None);
    let both = Report {
        rewritten: 2,
        unresolved: vec!["bin/x: source not found to vendor".into()],
        ..Report::default()
    };
    assert_eq!(
        relocate_headline(&both).as_deref(),
        Some("atpkg relocate: rewrote 2 object(s)")
    );
    assert_eq!(
        relocate_headline(&Report::default()).as_deref(),
        Some("atpkg relocate: no machine-local references found (already portable)")
    );
}

/// The synthetic-home rule, on its inputs: the same path is the real machine; any
/// other path is not, and the reason names both so a `sudo -H` reader learns why.
/// `defaults` ignores `$HOME` (measured 2026-09-14), which is why this exists.
#[test]
fn a_redirected_home_is_a_synthetic_machine() {
    use std::path::Path;
    assert_eq!(
        machine_home_mismatch_between(Path::new("/Users//me"), Path::new("/Users//me")),
        None
    );
    let why = machine_home_mismatch_between(Path::new("/tmp/fixture"), Path::new("/Users//me"))
        .expect("a temp HOME is not the account home");
    assert!(
        why.contains("/tmp/fixture") && why.contains("/Users//me"),
        "{why}"
    );
    assert!(why.contains("follow the account"), "{why}");
}

/// FAIL-CLOSED: an account that cannot be resolved is not proof that HOME is it.
/// (Some, Some) same → account; different → mismatch naming both; (Some, None) →
/// unresolved, naming the reason; (None, _) → the account (an unset HOME falls back
/// to the passwd entry in `home_dir()`).
#[test]
fn an_unresolvable_account_is_a_synthetic_machine() {
    use crate::machine::HomePosture;
    use std::path::Path;
    let me = Path::new("/Users//me");
    let fixture = Path::new("/tmp/fixture");
    assert_eq!(
        machine_home_verdict(Some(me), Some(me), false),
        (HomePosture::Account, None)
    );
    assert_eq!(
        machine_home_verdict(Some(fixture), Some(me), true).0,
        HomePosture::Account,
        "the filesystem's answer wins over the spelling"
    );
    let (p, why) = machine_home_verdict(Some(fixture), Some(me), false);
    assert_eq!(p, HomePosture::Mismatch);
    let why = why.unwrap();
    assert!(
        why.contains("/tmp/fixture") && why.contains("/Users//me"),
        "{why}"
    );
    let (p, why) = machine_home_verdict(Some(fixture), None, false);
    assert_eq!(p, HomePosture::Unresolved);
    assert!(why.unwrap().contains("could not be resolved"));
    assert_eq!(
        machine_home_verdict(None, Some(me), false).0,
        HomePosture::Account
    );
    assert_eq!(
        machine_home_verdict(None, None, false).0,
        HomePosture::Account
    );
}

/// The same directory under another spelling IS the real home; a different one, or
/// a missing one, is not.
#[cfg(unix)]
#[test]
fn the_same_directory_under_another_spelling_is_the_real_home() {
    // Unique per run, like every other temp fixture in this crate (`crate::testdir`):
    // with fixed path strings a second run of this suite on the same machine raced this
    // one on one directory and one symlink name, and the loser panicked in
    // `symlink(...).unwrap()`.
    let t = crate::testdir::dir("atpkg-same-directory-test-");
    let links = crate::testdir::dir("atpkg-same-directory-link-");
    let link = links.join("link");
    std::os::unix::fs::symlink(&t, &link).unwrap();
    assert!(same_directory(&link, &t), "a symlink to it is it");
    assert!(same_directory(&t.join("."), &t), "a dotted spelling is it");
    assert!(
        !same_directory(&t, &std::env::temp_dir()),
        "the parent is not it"
    );
    assert!(
        !same_directory(&t.join("missing"), &t),
        "a missing path proves nothing"
    );
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&t);
}

/// A CONFIG THAT DOES NOT PARSE CHANGES NOTHING. Both `[machine]` defaults ACT, so
/// "malformed ⇒ defaults" meant a typo three tables away silently re-enabled what the
/// owner had switched off. The refusal is first — before the home question — because
/// it is the cheaper one and the one the owner can fix.
#[test]
fn an_unreadable_config_refuses_before_anything_is_measured() {
    let unreadable = crate::config::MachineConfig {
        unreadable: true,
        ..crate::config::MachineConfig::default()
    };
    let reason = machine_refusal(&unreadable).expect("an unreadable config refuses");
    assert!(reason.contains("does not parse"), "{reason}");
    assert!(reason.contains("aterm pkg machine apply"), "{reason}");
    // And it refuses even though the resolved values look like the defaults.
    assert!(unreadable.spotlight_noindex());
    assert_eq!(
        unreadable.universal_control(),
        crate::config::UniversalControlPolicy::Off
    );
}

/// `machine`'s grammar is two spellings and nothing else: bare (read) and `apply`.
/// Anything more is a usage error, so a typo never silently reads when someone meant
/// to apply.
#[test]
fn machine_grammar_is_exact() {
    // Both accepted spellings are answered by `--help` without acting, like every
    // verb; and a third word is refused. The read/apply bodies touch the real
    // machine, so they are exercised by hand, not here.
    let argv = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    assert_eq!(
        cmd_machine(&argv("check")),
        ExitCode::from(2),
        "`check` is not a word: bare is the read"
    );
    assert_eq!(cmd_machine(&argv("apply --dry-run")), ExitCode::from(2));
    assert_eq!(cmd_machine(&argv("apply extra")), ExitCode::from(2));
    assert!(verb_usage("machine").is_some_and(|u| u.contains("apply")));
}

/// The `noindex` grammar, as a table: the default subcommand (`scan`), the default
/// root (the cwd — left `None` by the parser so it stays pure), the default depth,
/// and the part actually worth pinning — a flag on the WRONG subcommand is a usage
/// error, not a silent ignore.
///
/// A table rather than a run, because the alternative is discovering this grammar by
/// invoking a verb whose `verify` form waits 20 seconds on Spotlight and whose
/// `migrate` form renames a directory.
#[test]
fn noindex_grammar_is_exact() {
    let argv = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let job = |s: &str| parse_noindex(&argv(s)).ok();
    let scan = |root: Option<&str>, depth: usize| {
        Some(NoindexJob::Scan {
            root: root.map(ToOwned::to_owned),
            depth,
        })
    };
    // Bare `noindex` is a scan of the cwd at the verb's own depth — the subcommand
    // and the root are both optional, and `scan` is the default.
    assert_eq!(job(""), scan(None, crate::noindex::VERB_DEPTH));
    assert_eq!(job("scan"), scan(None, crate::noindex::VERB_DEPTH));
    assert_eq!(job("/src"), scan(Some("/src"), crate::noindex::VERB_DEPTH));
    assert_eq!(job("scan /src --depth 2"), scan(Some("/src"), 2));
    assert_eq!(job("--depth 2 /src"), scan(Some("/src"), 2));
    assert_eq!(
        job("verify /src/target"),
        Some(NoindexJob::Verify {
            dir: String::from("/src/target")
        })
    );
    assert_eq!(
        job("migrate /src/target --dry-run"),
        Some(NoindexJob::Migrate {
            dir: String::from("/src/target"),
            dry_run: true
        })
    );
    // A directory named like a subcommand is still a directory once the subcommand
    // slot is filled.
    assert_eq!(
        job("migrate scan"),
        Some(NoindexJob::Migrate {
            dir: String::from("scan"),
            dry_run: false
        })
    );
    // `apply`: `--all` (the doctor's scan) or any number of directories, dry-runnable.
    assert_eq!(
        job("apply --all"),
        Some(NoindexJob::Apply {
            all: true,
            dirs: vec![],
            dry_run: false
        })
    );
    assert_eq!(
        job("apply --all --dry-run"),
        Some(NoindexJob::Apply {
            all: true,
            dirs: vec![],
            dry_run: true
        })
    );
    assert_eq!(
        job("apply /a/target /b/target"),
        Some(NoindexJob::Apply {
            all: false,
            dirs: vec![String::from("/a/target"), String::from("/b/target")],
            dry_run: false
        })
    );
    for wrong in [
        // `apply` wants --all or directories, never both, never neither, no --depth.
        "apply",
        "apply --all /a",
        "apply /a --depth 2",
        "apply --all --depth 2",
        "scan --all",
        "migrate /d --all",
        // A flag on the wrong subcommand: refused, never accepted-and-ignored. The
        // ignored spelling would answer "nothing would change" about a form that
        // changes nothing anyway — true, and the wrong answer to what was asked.
        "scan --dry-run",
        "verify /d --dry-run",
        "verify /d --depth 3",
        "migrate /d --depth 3",
        // The operand these two cannot do without. There is no sensible default
        // directory to rename or probe.
        "verify",
        "migrate",
        // `--depth` wants a number in range and says so, rather than falling back to
        // the default: `--depth 0` would walk nothing and report a clean tree.
        "--depth",
        "--depth 0",
        "--depth 33",
        "--depth six",
        // Unknown flags and second operands are errors, not leftovers.
        "--json",
        "scan /a /b",
    ] {
        assert!(
            parse_noindex(&argv(wrong)).is_err(),
            "`atpkg noindex {wrong}` must be a usage error, not a silent default"
        );
    }
}

/// `noindex`'s `args[1]` is a SUBCOMMAND, not a program name, so it must stay out of
/// [`NAME_TAKING_VERBS`] exactly as `relocate` does. In that table the dispatch edge
/// refuses any unlisted `-`-leading first operand before the verb body runs, so
/// `atpkg noindex --depth 3` would die as "unknown option for `atpkg noindex`" while
/// `--depth` is a documented flag of the verb — the same shape as the audit finding
/// where `--elevate` was real, documented, and invisible at the edge.
#[test]
fn noindex_owns_its_own_flags_at_the_dispatch_edge() {
    assert!(
        !NAME_TAKING_VERBS.iter().any(|(name, _)| *name == "noindex"),
        "noindex reads args[1] as a subcommand, not a program name"
    );
    assert!(
        parse_noindex(&[String::from("--depth"), String::from("3")]).is_ok(),
        "a leading flag is the verb's own business"
    );
}

/// `--help` must ANSWER, never act. `gc --help` used to reclaim the rollback
/// window; a third of the audited surface did something when handed `--help`.
#[test]
fn help_flag_is_answered_for_every_verb_and_never_dispatches() {
    for verb in VERBS {
        for spelling in ["--help", "-h"] {
            let argv: Vec<std::ffi::OsString> = vec![(*verb).into(), spelling.into()];
            // Reaching a verb body would need the store lock or the network;
            // returning SUCCESS here is the observable proof it short-circuited.
            let code = main_entry(argv);
            assert_eq!(
                format!("{code:?}"),
                format!("{:?}", ExitCode::SUCCESS),
                "`atpkg {verb} {spelling}` must answer with usage and exit 0"
            );
        }
    }
}

/// A `--help` that belongs to the TOOL is forwarded, not answered: `run` looks
/// only at its first operand, and a literal `--` ends the scan for every verb.
#[test]
fn a_tools_own_help_flag_is_forwarded_by_run_and_stopped_by_a_separator() {
    let s = |xs: &[&str]| xs.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert!(help_flag_addresses_atpkg("run", &s(&["--help"])));
    assert!(help_flag_addresses_atpkg("run", &s(&["-h"])));
    assert!(!help_flag_addresses_atpkg("run", &s(&["rg", "--help"])));
    assert!(!help_flag_addresses_atpkg(
        "run",
        &s(&["rg", "--", "--help"])
    ));
    assert!(!help_flag_addresses_atpkg("run", &s(&[])));
    assert!(help_flag_addresses_atpkg("install", &s(&["ay", "--help"])));
    assert!(!help_flag_addresses_atpkg("install", &s(&["--", "--help"])));
    assert!(!help_flag_addresses_atpkg("install", &s(&[])));
}

/// THE single-writer verb roster ([`crate::lock`]): every verb that mutates
/// the store (stage/activate/discard, shims, links, pins, gc) takes the
/// store-wide lock at the dispatch edge; every read-only verb stays
/// lock-free. Exhaustive over the dispatch table so a new verb must be
/// classified deliberately.
#[test]
fn store_lock_verb_roster_is_exact() {
    // EXHAUSTIVE over VERBS: a verb added to the dispatch without a place in
    // one of these two lists fails here rather than silently defaulting to
    // lock-free. The `seed` verb landed while this test only enumerated the
    // verbs it already knew, so it classified nothing and proved nothing
    // about the new store mutator.
    let mut classified: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for mutator in [
        "install",
        "seed",
        "update",
        "rollback",
        "uninstall",
        "gc",
        "link",
        "unlink",
        "refresh",
        "pin",
        "unpin",
        // `repair` re-lays shims, so it writes the store and must hold the lock.
        "repair",
    ] {
        classified.insert(mutator);
        assert!(
            verb_mutates_store(mutator),
            "{mutator} mutates the store and must take the lock"
        );
    }
    for read_only in [
        "doctor",
        // `status` is the same read-only report under the name people reach for.
        "status",
        "which",
        "list",
        "run",
        "verify",
        "tree-root",
        "verify-index",
        "verify-pkg",
        "relocate",
        // `noindex` writes nothing under <prefix> — its probe file is removed by a
        // Drop guard and its one rename lives inside the tree the user named — so a
        // storage-hygiene scan never queues behind an install.
        "noindex",
        // `machine` writes a per-host preference and renames build output in the
        // user's own trees; nothing under <prefix> moves.
        "machine",
        // `lease` lays one file under `<prefix>/leases/` for the life of the command it
        // runs — beside the store, never in it — and must lease a toolchain while a
        // pass holds the store lock.
        "lease",
        // `lane` reads one command line and writes nothing anywhere.
        "lane",
        "help",
        "-h",
        "--help",
    ] {
        classified.insert(read_only);
        assert!(
            !verb_mutates_store(read_only),
            "{read_only} is read-only and must stay lock-free"
        );
    }
    let unclassified: Vec<&&str> = VERBS.iter().filter(|v| !classified.contains(*v)).collect();
    assert!(
        unclassified.is_empty(),
        "every advertised verb must be classified as a store mutator or read-only here \
         (a new verb defaults to lock-free, which is the dangerous direction): \
         {unclassified:?}"
    );
}

/// `lane` at the dispatch edge: a stock command line is refused (exit 2), a Trust
/// one is not (0), and a `--help` AFTER `--` belongs to the command — the edge that
/// answers `<verb> --help` stops at `--`, so `lane -- cargo --help` is read, not
/// answered with lane's own usage.
#[test]
fn lane_refuses_stock_rust_and_reads_the_command_after_the_separator() {
    let run = |argv: &[&str]| main_entry(argv.iter().map(std::ffi::OsString::from).collect());
    assert_eq!(
        run(&["lane", "--", "cargo", "+1.97.1", "test", "-p", "x"]),
        ExitCode::from(crate::reroute::REFUSAL_EXIT)
    );
    assert_eq!(
        run(&["lane", "--", "cargo", "--help"]),
        ExitCode::from(crate::reroute::REFUSAL_EXIT)
    );
    assert_eq!(
        run(&["lane", "--", "targo", "--unverified", "test", "-p", "x"]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        run(&["lane", "--", "ATERM_STOCK_REASON=wasm", "cargo", "build"]),
        ExitCode::SUCCESS
    );
    assert_eq!(run(&["lane", "--help"]), ExitCode::SUCCESS);
    assert_eq!(
        run(&["lane"]),
        ExitCode::from(1),
        "no command line is a usage error: no verdict, never a refusal"
    );
    assert_eq!(run(&["lane", "--verbose", "-"]), ExitCode::from(1));
}

/// A PROVISIONED MAC'S LAUNCH NEVER TOUCHES THE NETWORK FOR ITS SEEDLESS SENTENCE.
/// Every launch since v0.63.0 is seedless, and `report_seedless_posture` resolved the
/// signed index (a metered releases listing, verification, a floor record — under the
/// store lock) BEFORE asking whether anything was installed, though for a non-empty
/// store both outcomes printed the same line. The resolve now runs only for an empty
/// store, where its verdict is the point.
#[test]
fn the_seedless_lane_resolves_the_index_only_for_an_empty_store() {
    use std::cell::Cell;
    let calls = Cell::new(0u32);
    let resolve = |answer: Option<IndexServes>| {
        let calls = &calls;
        move || {
            calls.set(calls.get() + 1);
            answer
        }
    };
    let answers = || {
        [
            Some(IndexServes::Yes),
            Some(IndexServes::Unserved(Vec::new())),
            Some(IndexServes::Narrowed(Narrowing {
                excluded: vec!["ay".into()],
                removed: Vec::new(),
            })),
            None,
        ]
    };
    // A store that holds builds is served, whatever the index would have said — and
    // the index is never asked.
    for answer in answers() {
        assert_eq!(
            seedless_serves_us(true, resolve(answer)),
            Some(IndexServes::Yes)
        );
    }
    assert_eq!(
        calls.get(),
        0,
        "a provisioned store must not resolve the index for the seedless sentence"
    );
    // An empty store asks exactly once and reports what the index said.
    for answer in answers() {
        assert_eq!(seedless_serves_us(false, resolve(answer.clone())), answer);
    }
    assert_eq!(calls.get(), 4, "an empty store resolves once per verdict");

    // NON-VACUITY: the verb routes through the helper, feeds it the shim view, and
    // builds the fetcher and resolves the index only INSIDE the deferred closure.
    let src = include_str!("cli.rs");
    let production = production_half(src);
    let start = production
        .find("fn report_seedless_posture")
        .expect("the seedless posture reporter");
    let end = production[start..]
        .find("\nfn ")
        .map(|i| start + i)
        .expect("a function after report_seedless_posture");
    let body = &production[start..end];
    let routed = whole_calls(body, "seedless_serves_us(");
    assert_eq!(
        routed.len(),
        1,
        "report_seedless_posture must decide through seedless_serves_us: {body}"
    );
    let call = &routed[0];
    let holds = call
        .find("!crate::active_builds(layout).is_empty()")
        .expect("the store-holds-builds argument is the shim view");
    let fetcher = call
        .find("resolve_fetcher(layout)")
        .expect("the fetcher is built inside the call");
    assert!(
        holds < fetcher,
        "the store question is answered before the fetcher exists"
    );
    let outside = body.replace(call.as_str(), "");
    for costly in [
        "resolve_fetcher(",
        "resolve_verified_index(",
        "index_serves(",
        "seed_serviceable(",
    ] {
        assert!(
            !outside.contains(costly),
            "{costly} must run only inside the deferred resolve, never ahead of the \
             store question"
        );
    }
}

/// THE WINDOW'S OWN PASS IS NOT TOLD TO INSTALL WHAT IT IS INSTALLING (2026-09-23
/// audit, GT-15): the seed verb's "not installed yet … to start now: aterm pkg install
/// --default-set" was written at every launch into the window's log by the pass the
/// window starts, which downloads the set itself right behind it. Said to a person who
/// ran the verb over an empty store; never under the window's `--wait-lock` pass, and
/// never over a store that holds builds.
#[test]
fn the_not_installed_sentence_is_for_whoever_ran_the_verb() {
    assert!(
        says_not_installed_yet(true, false),
        "typed, empty store: said"
    );
    assert!(
        !says_not_installed_yet(true, true),
        "the window's pass: silent"
    );
    assert!(
        !says_not_installed_yet(false, false),
        "a stocked store: silent"
    );
    assert!(!says_not_installed_yet(false, true));
    // NON-VACUITY: the seed verb's sentence is decided through the helper, with the
    // window's pass read from the edge's `--wait-lock`.
    let src = include_str!("cli.rs");
    let production = production_half(src);
    let start = production
        .find("fn report_seedless_posture")
        .expect("the seedless posture reporter");
    let body = &production[start..];
    let body = &body[..body[3..].find("\nfn ").map_or(body.len(), |i| i + 3)];
    let routed = whole_calls(body, "says_not_installed_yet(");
    assert_eq!(routed.len(), 1, "{body}");
    assert!(routed[0].contains("wait_lock().is_some()"), "{}", routed[0]);
    let said = body
        .find("is not installed yet")
        .expect("the sentence is in the reporter");
    assert!(
        body[..said].contains("says_not_installed_yet("),
        "the sentence sits behind the decision"
    );
}

/// ADOPTION is what keeps a user's toolchain the WHOLE toolchain as the suite grows
/// (§11): under the one install consent (`auto_install`, default on) a machine that
/// runs the set gets newly-published members without being asked again.
/// The lifecycle is small enough to pin exactly, and getting it wrong is silent in both
/// directions — a missing marker decays the toolset, a sticky one reinstalls what the
/// user deliberately removed.
#[test]
fn adoption_is_recorded_by_the_whole_set_and_forgotten_by_uninstall() {
    let dir = crate::testdir::dir("atpkg-adopt-");
    let layout = crate::store::Layout {
        prefix: dir.clone(),
    };

    // A fresh store has NOT adopted: the consent question is still open.
    assert!(!adopted(&layout), "a fresh store has adopted nothing");

    // A whole-set install records it, and is idempotent.
    record_adoption(&layout);
    assert!(adopted(&layout), "the whole-set install adopts");
    let first = std::fs::read(layout.adopted()).unwrap();
    record_adoption(&layout);
    assert_eq!(
        std::fs::read(layout.adopted()).unwrap(),
        first,
        "re-adopting rewrites nothing"
    );

    // The marker is 0600 like every other durable store file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(layout.adopted())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "adoption marker must be 0600");
    }

    // Adoption is the file's EXISTENCE — never a parsed value, so a garbled or
    // truncated write can never read as some other answer.
    std::fs::write(layout.adopted(), b"\0\0garbage").unwrap();
    assert!(adopted(&layout), "existence is the whole predicate");

    // Uninstall forgets it: set-completion must never undo an explicit removal.
    clear_adoption(&layout);
    assert!(!adopted(&layout), "uninstall opts out of set completion");
    // Clearing what is already clear is not an error.
    clear_adoption(&layout);

    // The whole-set verb's decision table: an install adopts; a set found ALREADY
    // COMPLETE adopts (the re-adopt the uninstall message — and doctor's re-seed remedy
    // of the time — led to; it recorded nothing until 2026-09-14); a machine with no builds
    // before and none after, a failed member, a skipped member or an owed member does
    // not.
    let none: Vec<String> = Vec::new();
    let some = vec![String::from("trust")];
    assert!(
        default_set_adopts(1, 0, false, &none, &none),
        "installed: adopts"
    );
    assert!(
        default_set_adopts(1, 2, true, &some, &some),
        "installed something: adopts"
    );
    assert!(
        default_set_adopts(0, 0, true, &none, &none),
        "already complete: adopts"
    );
    assert!(
        !default_set_adopts(0, 0, false, &none, &none),
        "no toolset before or after: not adopted"
    );
    assert!(
        !default_set_adopts(0, 1, true, &none, &none),
        "a failure: not adopted"
    );
    assert!(
        !default_set_adopts(0, 0, true, &some, &none),
        "a skip: not adopted"
    );
    assert!(
        !default_set_adopts(0, 0, true, &none, &some),
        "owed: not adopted"
    );
    assert!(!adopted(&layout));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A TARGETED removal must stick too. The resumable seed lane installs whatever
/// the store lacks, so without a per-program record `atpkg uninstall ny` came
/// back on the next launch — the manager undoing a deliberate act, which is the
/// single most infuriating thing a package manager can do. Asking for it back
/// lifts the record, so nobody has to learn a marker file exists.
#[test]
fn a_single_program_removal_is_not_undone_by_the_seed_lane() {
    let dir = crate::testdir::dir("atpkg-rm-");
    let layout = crate::store::Layout {
        prefix: dir.clone(),
    };

    assert!(removed_programs(&layout).is_empty());
    record_removed(&layout, "ny");
    record_removed(&layout, "clean");
    record_removed(&layout, "ny"); // idempotent
    let rm = removed_programs(&layout);
    assert!(rm.contains("ny") && rm.contains("clean"), "{rm:?}");
    assert_eq!(rm.len(), 2, "no duplicate from the repeat: {rm:?}");
    // The comment header must never read as a program name.
    assert!(!rm.iter().any(|p| p.starts_with('#')), "{rm:?}");

    clear_removed(&layout, &["ny".to_string()]);
    let rm = removed_programs(&layout);
    assert!(
        !rm.contains("ny"),
        "an explicit install lifts just that one"
    );
    assert!(rm.contains("clean"), "and leaves the others alone");

    clear_removed(&layout, &["clean".to_string()]);
    assert!(
        removed_programs(&layout).is_empty(),
        "emptying the set removes the file rather than leaving a header behind"
    );
    assert!(!layout.removed().exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A removal that could NOT be recorded must be SAID. The write is already
/// temp+rename, so a kill cannot shorten the list — but an unwritable prefix (a
/// read-only store, a full disk, a `removed` path something else now owns) left
/// `uninstall` printing "uninstalled ay" and exiting 0 with nothing recorded, and
/// this file is the only thing keeping the program out of the unattended lanes:
/// `seed_serviceable` and `install_default_set_inner` both filter on
/// `removed_programs`, and `cmd_seed` records adoption again on every launch, so the
/// next one reinstalls exactly what the user removed. The failure now comes back
/// from `write_removed`, the recorder prints it, and the entries already recorded
/// survive it — the rename either replaces the record or leaves it alone.
#[cfg(unix)]
#[test]
fn a_removal_record_that_cannot_be_written_is_reported_not_swallowed() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::testdir::dir("atpkg-rm-ro-");
    let layout = crate::store::Layout {
        prefix: dir.clone(),
    };

    record_removed(&layout, "ny");
    assert!(removed_programs(&layout).contains("ny"));

    // The prefix goes read-only: the temp file cannot be created, and the rename
    // that would publish it could not land either. Permissions are restored before
    // anything is asserted, so a failure here still leaves a removable directory.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut all = removed_programs(&layout);
    all.insert("clean".to_string());
    let published = write_removed(&layout, &all);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        published.is_err(),
        "an unwritable prefix must come back as an error, never a silent success"
    );

    // The removal already recorded is intact — never truncated in place — and no
    // half-written temp file is left for the next reader to find.
    assert_eq!(
        removed_programs(&layout),
        std::collections::BTreeSet::from(["ny".to_string()]),
        "a failed write must not cost the removals already recorded"
    );
    let strays: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "removed")
        .collect();
    assert!(
        strays.is_empty(),
        "no temp file may survive the failure: {strays:?}"
    );

    // And the failure is SURFACED, not merely returned: `record_removed` is called
    // from `uninstall_and_retire`, whose caller prints "uninstalled {program}" and
    // exits 0 whatever happened, so stderr is the only place the loss can appear.
    let src = include_str!("cli.rs");
    let start = src.find("fn record_removed(").expect("the recorder");
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("its end");
    assert!(
        src[start..end].contains("eprintln!("),
        "a removal that could not be recorded must be printed, like every other \
         marker write's failure"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// REMOVAL MUST STICK. The seed lane is resumable — it installs whatever the
/// store lacks, so an interrupted first run can finish later — and that same
/// property means it would happily reinstall everything `uninstall --all` just
/// removed, on the very next launch. The decline marker is what stops that, and
/// an explicit install is what lifts it. Both directions are pinned here because
/// getting either wrong is user-hostile in a way tests are the only defence
/// against: one way the manager fights the user, the other way a change of mind
/// requires deleting a file nobody documented.
#[test]
fn declining_the_toolset_survives_until_an_explicit_install() {
    let dir = crate::testdir::dir("atpkg-decline-");
    let layout = crate::store::Layout {
        prefix: dir.clone(),
    };

    assert!(!declined(&layout), "a fresh store has declined nothing");
    record_decline(&layout);
    assert!(declined(&layout), "uninstall --all declines the toolset");
    // Idempotent, and 0600 like every other durable marker.
    record_decline(&layout);
    assert!(declined(&layout));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(layout.declined())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "decline marker must be 0600");
    }
    // Existence is the predicate — never a parsed value.
    std::fs::write(layout.declined(), b"\0garbage").unwrap();
    assert!(declined(&layout));

    clear_decline(&layout);
    assert!(!declined(&layout), "an explicit install lifts the decline");
    clear_decline(&layout); // clearing what is clear is not an error

    // Decline and adoption are INDEPENDENT facts: declining must not read as
    // adoption, or a removed toolset would still get set-completion.
    record_decline(&layout);
    assert!(!adopted(&layout), "declining is not adopting");

    let _ = std::fs::remove_dir_all(&dir);
}

/// THE WIRE ANNOUNCEMENT IS PURE, so its words stay pinned by a test: a non-empty
/// pass announces before acting with `net-starting:` and the set it will attempt,
/// and the ordinary no-op tick stays silent whoever consented. (It was lane-gated
/// until Phase 5 deleted the sealed-payload lane, whose local extraction it once
/// announced as "over the network".)
#[test]
fn the_wire_announcement_speaks_only_for_a_non_empty_pass() {
    let two = vec!["ay".to_string(), "trust".to_string()];
    let line = net_announcement(Consent::InstallingAterm, &two, SignedSizes::UNKNOWN)
        .expect("a non-empty network pass announces before acting");
    assert!(
        line.starts_with(&format!("atpkg: {NET_STARTING_MARKER}")),
        "the GUI strips exactly this prefix: {line}"
    );
    assert!(line.contains("ay, trust"), "{line}");
    let known = SignedSizes {
        download: Some(1 << 28),
        disk: Some(1 << 30),
    };
    for consent in [Consent::InstallingAterm, Consent::Explicit] {
        assert!(net_announcement(consent, &[], known).is_none());
    }
    // NON-VACUITY: the wire marker reaches stdout through `net_announcement` alone,
    // and the seed verb — which installs nothing — never announces.
    let production = production_half(include_str!("cli.rs"));
    let start = production.find("\nfn cmd_seed(").expect("the seed verb");
    let end = production[start + 1..]
        .find("\nfn ")
        .map(|i| start + 1 + i)
        .expect("a function after cmd_seed");
    assert!(
        !production[start..end].contains("STARTING"),
        "the seed verb installs nothing, so it announces nothing"
    );
    assert_eq!(
        production.matches("NET_STARTING_MARKER}").count(),
        1,
        "the wire announcement has exactly ONE production emission site \
         (inside net_announcement)"
    );
}

/// THE MARKER CONTRACT HAS A POSITIVE TERMINAL, and the ledger knows which half
/// of the contract each marker keeps.
///
/// Until 2026-09-11 every terminal in this contract reported a PROBLEM
/// (`seed-failed:`, `seed-partial:`, `seed-unusable:`, `net-failed:`) beside the
/// two install rosters, so a pass that announced itself, ended well and had no
/// roster to name printed nothing at all — and its only reader, the GUI, had to
/// infer an outcome from that silence. It inferred "failed", and put "⚠ ALab
/// toolchain install failed" over a clean run on a healthy machine.
#[test]
fn the_marker_table_covers_every_marker_and_names_the_one_that_opens() {
    use super::announcement::MARKERS;
    let opens: Vec<&str> = MARKERS
        .iter()
        .filter(|(_, o)| *o)
        .map(|(m, _)| *m)
        .collect();
    assert_eq!(
        opens,
        vec![NET_STARTING_MARKER],
        "exactly one marker OPENS an announcement; every other one answers"
    );
    assert!(
        MARKERS.iter().any(|(m, _)| *m == SEED_DONE_MARKER),
        "the positive terminal must be in the table or it answers nothing"
    );
    // NON-VACUITY: every marker const this module publishes is classified. A new
    // marker that is not in the table silently stops answering announcements —
    // the exact failure mode this contract's own doc comment warns about.
    for marker in [
        SEED_UNUSABLE_MARKER,
        SEED_FAILED_MARKER,
        SEED_DONE_MARKER,
        SEED_BUSY_MARKER,
        NET_STARTING_MARKER,
        NET_INSTALLED_MARKER,
        NET_FAILED_MARKER,
    ] {
        assert!(
            MARKERS.iter().any(|(m, _)| *m == marker),
            "{marker:?} is published as part of the contract but classified nowhere"
        );
    }
    // …and `lock-waiting:`, which is NEITHER half, stays out of the table: a
    // `lock-waiting:` line answers nothing and opens nothing, so the ledger
    // must read straight past it (a child that waited and then announced is
    // an OPEN announcement, not an answered one).
    assert!(
        !MARKERS.iter().any(|(m, _)| *m == LOCK_WAITING_MARKER),
        "`lock-waiting:` is neither an announcement nor an answer"
    );
    // …and so does `lock-acquired:`, which answers the WAIT, not an announcement:
    // were it an answer, a child that waited, acquired and then announced would
    // look answered before it began.
    assert!(
        !MARKERS.iter().any(|(m, _)| *m == LOCK_ACQUIRED_MARKER),
        "`lock-acquired:` is neither an announcement nor an answer"
    );
    use super::announcement;
    announcement::reset();
    announcement::note(&format!(
        "atpkg: {LOCK_WAITING_MARKER}another atpkg process holds the store lock"
    ));
    assert!(!announcement::is_open());
    announcement::note(&format!(
        "atpkg: {LOCK_ACQUIRED_MARKER}the other process finished"
    ));
    assert!(!announcement::is_open());
    announcement::note(&format!(
        "atpkg: {NET_STARTING_MARKER}installing 2 program(s)"
    ));
    assert!(
        announcement::is_open(),
        "a wait before the announcement did not pre-answer it"
    );
    announcement::reset();
}

/// The ledger reads the same bytes the GUI parses, and an INFORMATIONAL row is not
/// an answer. `managed-current:` / `machine-settings:` are printed at the end of
/// every pass, announced or not; counting one as a terminal is round 9's rule read
/// backwards, and it would let a pass retire a card it had not finished with.
#[test]
fn only_a_terminal_marker_closes_an_announcement() {
    use super::announcement;
    announcement::reset();
    assert!(!announcement::is_open(), "nothing announced, nothing open");

    announcement::note("atpkg: this is an ordinary sentence");
    assert!(!announcement::is_open());

    announcement::note(&format!(
        "atpkg: {NET_STARTING_MARKER}installing 2 program(s)"
    ));
    assert!(announcement::is_open(), "the announcement opened");

    announcement::note("atpkg: managed-current: claude 2.1.281 (Anthropic latest)");
    announcement::note("atpkg: machine-settings: universal-control disabled");
    assert!(
        announcement::is_open(),
        "an R6 row says what the machine HAS, not how the pass ended"
    );

    announcement::note(&format!("atpkg: {SEED_DONE_MARKER}the pass finished"));
    assert!(!announcement::is_open(), "the positive terminal answers");
    announcement::reset();
}

/// THE ROUND-10 PROPERTY, at its source. On a provisioned Mac the passes announce nothing
/// — `atpkg seed` installs nothing, so it prints a plain sentence and exits 0, and `atpkg
/// update`'s plan is empty so `net_announcement` returns `None`. Nothing may be printed
/// at the end of those passes, or the warning card is back on screen at every launch of a
/// healthy machine (2026-08-20 round-10 audit).
#[test]
fn a_pass_that_announced_nothing_answers_nothing() {
    use super::announcement;
    announcement::reset();
    // The steady state's own words, verbatim in shape: no marker anywhere.
    announcement::note("atpkg: the store already holds the ALab toolset");
    assert!(!announcement::is_open());
    // `answer_announcement` is what runs at every announcing verb's tail; with no
    // announcement open it must be a no-op, for BOTH verdicts.
    for ok in [true, false] {
        answer_announcement(ok);
        assert!(
            !announcement::is_open(),
            "a verb that announced nothing has nothing to answer (ok={ok})"
        );
    }
    announcement::reset();
}

/// Every `<head>…)` invocation in `src`, brace/paren-matched so a wrapped macro
/// call is returned whole. String literals are skipped so a `)` inside one cannot
/// close the call early.
fn whole_calls(src: &str, head: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = src[from..].find(head) {
        let start = from + rel;
        let mut i = start + head.len();
        let mut depth = 1usize;
        let mut in_str = false;
        while i < bytes.len() {
            let c = bytes[i];
            if in_str {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        out.push(src[start..i.min(bytes.len())].to_string());
        from = start + head.len();
    }
    out
}

/// The wire lane says what it is about to do, and how to stop it: the lean app has no
/// seal, so the whole set arrives here on every current-release Mac. The way out is timed
/// honestly — `uninstall --all` cannot run until the pass releases the store lock, so the
/// line says "once this pass ends" rather than sending the reader into that refusal.
#[test]
fn the_network_lane_names_the_size_and_the_off_switches() {
    // The set a first launch names, from the compiled lists the client installs from,
    // sorted as `install_default_set_inner` sorts `will_install`.
    let mut set: Vec<String> = crate::stub::DEFAULT_SET_STUB_NAMES
        .iter()
        .map(|(n, _)| (*n).to_string())
        .chain(crate::stub::AGENT_PROGRAMS.iter().map(|n| (*n).to_string()))
        .collect();
    set.sort();
    set.dedup();
    // 3_650_000_000 B = 3.399… GiB and 1_100_000_000 B = 1.024… GiB, so the one
    // renderer's rounding is exercised on both figures of the same `[cost]` row.
    let both = SignedSizes {
        download: Some(1_100_000_000),
        disk: Some(3_650_000_000),
    };
    let line = net_announcement(Consent::InstallingAterm, &set, both)
        .expect("a non-empty network pass announces before acting");
    // The GUI strips exactly this prefix…
    assert!(
        line.starts_with(&format!(
            "atpkg: {NET_STARTING_MARKER}installing {} program(s) over the network: ay, \
             claude, ",
            set.len()
        )),
        "{line}"
    );
    // …each size is cost::human_bytes of its signed sum, stated once…
    assert_eq!(
        line.matches("3.4 GiB").count(),
        1,
        "one on-disk figure, once: {line}"
    );
    assert_eq!(
        line.matches("1.0 GiB").count(),
        1,
        "one download figure, once: {line}"
    );
    // …both off switches are named, spelled as typed…
    assert!(line.contains("`aterm pkg uninstall --all`"), "{line}");
    assert!(line.contains("`[packages].auto_install = false`"), "{line}");
    // …the uninstall verb is timed honestly — refused while the pass holds the store
    // lock — so the line says "once this pass ends" before naming it…
    let ends = line.find("once this pass ends").expect("the timing");
    let verb = line.find("`aterm pkg uninstall --all`").expect("the verb");
    assert!(
        ends < verb,
        "the timing must qualify the verb, not trail it: {line}"
    );
    assert!(line.contains("records the decline"), "{line}");
    assert!(
        !line.contains("opt out with `aterm pkg uninstall"),
        "untimed: {line}"
    );
    // …the whole line owns exactly two '(' — "program(s)" and the size — so a
    // parenthesis can arrive from neither the switch phrase nor the format…
    assert_eq!(line.matches('(').count(), 2, "{line}");
    // …and the sizes are the line's trailing parenthetical, which is where the toolchain
    // bar reads them, so the off-switch phrase must bring no parenthesis of its own…
    let (_, tail) = line.rsplit_once('(').expect("a parenthetical");
    assert_eq!(tail, "~1.0 GiB download, ~3.4 GiB on disk)", "{line}");
    let (consent_clause, seed_clause) = Consent::InstallingAterm.announced();
    for phrase in [NET_OFF_SWITCH, consent_clause, seed_clause] {
        assert!(
            !phrase.contains(['(', ')']),
            "the bar would read the wrong parenthetical: {phrase}"
        );
    }
    // …shown whole: the bar clips the text inside the parentheses at 40 characters, and
    // the figures this line exists to disclose may not be what gets clipped…
    let inside = tail.strip_suffix(')').expect("closed");
    assert!(
        inside.chars().count() <= 40,
        "{inside:?} would be clipped on the toolchain bar"
    );
    assert_eq!(crate::progress::sanitize_for_tty(inside, 40), inside);
    // …and the whole line fits the log, whose sink elides a record body past
    // aterm_log::MAX_RECORD_BYTES = 512. The sizes are last on the line, so they are the
    // first casualty of any growth. atpkg does not depend on aterm-log, so the cap is
    // pinned here by hand: change both or neither.
    const MAX_RECORD_BYTES: usize = 512; // = aterm_log::MAX_RECORD_BYTES
    let detail = line
        .strip_prefix(&format!("atpkg: {NET_STARTING_MARKER}"))
        .expect("the GUI strips exactly this prefix");
    let logged = "atpkg install pass starting: ".len() + detail.len();
    assert!(
        logged <= MAX_RECORD_BYTES,
        "the log sink would elide the sizes: {logged} bytes as logged, cap \
         {MAX_RECORD_BYTES}: {line}"
    );
    // The explicit verb is its own consent: it never reads `auto_install`, so its line is
    // the same line with exactly those two clauses gone — the sizes, the timed uninstall
    // switch and the two '(' all stay.
    let explicit = net_announcement(Consent::Explicit, &set, both)
        .expect("a non-empty network pass announces before acting");
    assert!(
        !explicit.contains("installing aterm is the consent"),
        "{explicit}"
    );
    assert!(!explicit.contains("auto_install"), "{explicit}");
    assert_eq!(
        explicit,
        line.replacen(consent_clause, "", 1)
            .replacen(seed_clause, "", 1),
        "the explicit line differs by exactly the two clauses"
    );
    assert!(
        explicit.ends_with(&format!(
            " — {NET_OFF_SWITCH} (~1.0 GiB download, ~3.4 GiB on disk)"
        )),
        "{explicit}"
    );
    assert_eq!(explicit.matches('(').count(), 2, "{explicit}");
    // One figure known prints that figure alone, in its own tier: 970_000_000 B is under
    // 1 GiB, so the renderer says "925.1 MiB". The two figures need not share a unit, and
    // neither is rounded up to match the other.
    let disk_only = net_announcement(
        Consent::InstallingAterm,
        &set,
        SignedSizes {
            download: None,
            disk: Some(3_650_000_000),
        },
    )
    .expect("announces");
    assert!(
        disk_only.ends_with(" (~3.4 GiB on disk when finished)"),
        "{disk_only}"
    );
    assert_eq!(disk_only.matches("GiB").count(), 1, "{disk_only}");
    let download_only = net_announcement(
        Consent::InstallingAterm,
        &set,
        SignedSizes {
            download: Some(970_000_000),
            disk: None,
        },
    )
    .expect("announces");
    assert!(
        download_only.ends_with(" (~925.1 MiB download)"),
        "{download_only}"
    );
    assert_eq!(download_only.matches("MiB").count(), 1, "{download_only}");
    assert!(!download_only.contains("GiB"), "{download_only}");
    // No signed sum ⇒ no size at all (no number beats a wrong one), the
    // switches still named, and the last '(' is then "program(s)", whose
    // tail does not close — the bar shows "starting…" rather than a guess.
    let bare = net_announcement(Consent::InstallingAterm, &set, SignedSizes::UNKNOWN)
        .expect("a non-empty network pass announces before acting");
    assert!(!bare.contains("GiB") && !bare.contains("GB"), "{bare}");
    assert!(bare.contains(NET_OFF_SWITCH), "{bare}");
    let (_, tail) = bare.rsplit_once('(').expect("program(s)");
    assert!(!tail.ends_with(')'), "{bare}");
}

/// Who adopted decides what the line may claim: "installing aterm is the consent", with
/// `auto_install = false` as the switch, is true of every update pass except on a machine
/// a person adopted with the explicit verb (the marker without the seed pass's record).
/// The record's own life cycle is
/// `who_adopted_is_recorded_only_when_the_seed_pass_creates_adoption`.
#[test]
fn only_a_machine_the_explicit_verb_adopted_drops_the_first_launch_consent() {
    // (adopted, adopted_by_seed) → the consent the line states.
    assert_eq!(set_completion_consent(true, true), Consent::InstallingAterm);
    assert_eq!(
        set_completion_consent(false, false),
        Consent::InstallingAterm,
        "no pass asks this (completion needs adoption), and the answer is the default"
    );
    assert_eq!(
        set_completion_consent(true, false),
        Consent::Explicit,
        "the explicit verb adopted this machine: its own consent"
    );
    assert_eq!(
        set_completion_consent(false, true),
        Consent::InstallingAterm,
        "a stray record without the marker vouches for nothing either way"
    );

    // Up to the verb's own closing brace, not the next top-level `fn`: the items between
    // `cmd_update_all` and the next function include `Consent` itself, whose arms would
    // satisfy any search. Each verb wraps a `_code` worker, so the scans read the worker.
    fn body_of(src: &'static str, signature: &str) -> &'static str {
        let start = src.find(signature).expect("the verb");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .expect("the verb's closing brace");
        &src[start..end]
    }
    let src = include_str!("cli.rs");
    let verb = body_of(src, "fn install_toolset(");
    assert!(
        verb.contains("Consent::Explicit"),
        "the explicit verb is its own consent"
    );
    assert!(
        !verb.contains("Consent::InstallingAterm"),
        "the explicit verb must never claim the first launch's consent"
    );
    let update = body_of(src, "fn run_update_pass(");
    // Whitespace-free, so the formatter's line breaks cannot hide the call.
    let squeezed: String = update.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        squeezed.contains("set_completion_consent(adopted(layout),adopted_by_seed(layout))"),
        "the update pass asks which consent holds on this machine, including who adopted it"
    );
    assert!(
        !update.contains("Consent::InstallingAterm") && !update.contains("Consent::Explicit"),
        "the update pass must not hard-code a consent"
    );
    // Who adopted is written by the seed verb alone and cleared by the explicit one.
    assert!(
        verb.contains("clear_seed_adoption(layout)") && !verb.contains("record_seed_adoption"),
        "the explicit verb takes the seed pass's claim over and never writes it"
    );
    let seed = body_of(src, "fn cmd_seed(");
    assert!(
        seed.contains("record_seed_adoption(&layout)")
            && !seed.contains("record_adoption(&layout)"),
        "the seed pass adopts through the door that records who adopted"
    );
}

/// Who adopted, across the verbs that change it: the record behind
/// `set_completion_consent`'s second input. The seed pass writes it only when it creates
/// adoption, since stamping a marker it found would hand a machine the explicit verb
/// adopted back to the first launch's consent. Neither file alone vouches for anything.
#[test]
fn who_adopted_is_recorded_only_when_the_seed_pass_creates_adoption() {
    let layout = temp_layout("who-adopted");
    // What `cmd_update_all` asks.
    let consent = |layout: &crate::store::Layout| {
        set_completion_consent(adopted(layout), adopted_by_seed(layout))
    };

    // The first launch adopts, and says so.
    record_seed_adoption(&layout);
    assert!(adopted(&layout) && adopted_by_seed(&layout));
    assert_eq!(consent(&layout), Consent::InstallingAterm);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(layout.adopted_by_seed())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the record is 0600 like the marker");
    }
    // Every later launch re-runs the seed pass: a no-op.
    let first = std::fs::read(layout.adopted_by_seed()).unwrap();
    record_seed_adoption(&layout);
    assert_eq!(std::fs::read(layout.adopted_by_seed()).unwrap(), first);

    // Default keys, `uninstall --all`, then Settings "Install ALab toolset":
    // uninstall forgets both files, the explicit verb clears the record and
    // adopts, and the next launch's seed pass finds a marker and records nothing.
    clear_adoption(&layout);
    assert!(!adopted(&layout) && !layout.adopted_by_seed().exists());
    clear_seed_adoption(&layout);
    record_adoption(&layout);
    record_seed_adoption(&layout);
    assert!(adopted(&layout) && !adopted_by_seed(&layout));
    assert_eq!(
        consent(&layout),
        Consent::Explicit,
        "the explicit verb adopted this machine; its passes must not claim the first launch"
    );

    // The explicit verb on a machine the seed pass adopted takes the claim over.
    clear_adoption(&layout);
    record_seed_adoption(&layout);
    assert_eq!(consent(&layout), Consent::InstallingAterm);
    clear_seed_adoption(&layout);
    assert!(adopted(&layout), "clearing the record is not un-adopting");
    assert_eq!(consent(&layout), Consent::Explicit);
    // Clearing what is already clear is not an error.
    clear_seed_adoption(&layout);

    // A record without the marker vouches for nothing…
    clear_adoption(&layout);
    std::fs::write(layout.adopted_by_seed(), b"stray").unwrap();
    assert!(!adopted_by_seed(&layout));
    // …and a marker that predates the record reads as the explicit verb's.
    clear_seed_adoption(&layout);
    record_adoption(&layout);
    assert_eq!(consent(&layout), Consent::Explicit);

    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The None-on-zero sum behind both figures: one undeclared (zero) entry
/// poisons its total, the other figure is unaffected, and an already-poisoned
/// total stays `None` whatever follows.
#[test]
fn a_signed_sum_is_poisoned_by_one_undeclared_entry() {
    assert_eq!(sum_nonzero(Some(0), 5), Some(5));
    assert_eq!(sum_nonzero(Some(5), 7), Some(12));
    assert_eq!(sum_nonzero(Some(5), 0), None);
    assert_eq!(sum_nonzero(None, 7), None);
    assert_eq!(sum_nonzero(Some(u64::MAX), 1), Some(u64::MAX));
    // The renderer prints only what is known.
    let known = SignedSizes {
        download: Some(1 << 30),
        disk: Some(1 << 30),
    };
    assert_eq!(
        size_parenthetical(known),
        " (~1.0 GiB download, ~1.0 GiB on disk)"
    );
    assert_eq!(size_parenthetical(SignedSizes::UNKNOWN), "");
}

/// THE CONSENT POLICY, as a table. Every consent regression this branch shipped
/// lived in this one boolean, and none of them had a test — reverting either
/// round-3 fix left the whole suite green, which is precisely why they shipped.
/// Inputs (2026-09-23): the ONE install consent, Automatic updates, the decline and
/// an unreadable table — adoption is no longer one.
#[test]
fn set_completion_honours_every_opt_out_over_the_one_consent() {
    // (consent, adopted, asked, enabled, declined, unreadable)
    // The batteries-included default on an adopted machine: keep the set complete.
    assert!(should_complete_set(true, true, false, true, false, false));
    // Not adopted — a fresh store before its seed pass, or a seed refusal that took
    // its adoption back for want of disk — installs nothing new.
    assert!(!should_complete_set(true, false, false, true, false, false));
    // …unless `auto_install = true` is WRITTEN: the old network-bootstrap consent, and
    // the one way a machine no window runs on gets the set (and gets it back after
    // `uninstall <program>` cleared adoption). Before this row, a terminal-only
    // machine with that line stopped installing when Phase 4 landed.
    assert!(
        should_complete_set(true, false, true, true, false, false),
        "a written `auto_install = true` stands in for adoption"
    );
    // `auto_install = false` (or the retired `seed_install = false`) stops it, even
    // on a machine adopted long ago — and so does a `[packages] prefix` this build
    // ignores (`installs_unattended` is the consent input).
    assert!(!should_complete_set(false, true, false, true, false, false));
    // Automatic updates off: no pass installs anything new, typed or not.
    assert!(
        !should_complete_set(true, true, true, false, false, false),
        "enabled = false always wins over the install consent"
    );
    // DECLINED outranks the consent — a written one too. Honouring a removal on the
    // local lane while the network pass quietly reinstalled the set made the decline
    // work for exactly one launch.
    assert!(
        !should_complete_set(true, true, true, true, true, false),
        "declining outranks the install consent — it is the later, more specific act"
    );
    // AN UNREADABLE [packages] TABLE OUTRANKS IT TOO (2026-09-16 audit). This lane is
    // narrowed by `[packages].exclude`, and a table that did not parse has none to
    // narrow it with: running it anyway reinstalls, over the network, the very
    // program the owner excluded — on a machine whose only mistake was a TOML typo,
    // possibly in the GUI-owned part of the file.
    assert!(
        !should_complete_set(true, true, true, true, false, true),
        "the set waits for the config to be readable before it is completed from an \
         exclude list nobody could read"
    );
}

/// THE WORK RULE the next-index probe now asks before it spends a HEAD (audit PK-7): an
/// empty store that is not completing the set has nothing an index could move, so its
/// `update` answers `EMPTY_UPDATE` and no published index may wake one. The controls: an
/// installed program, or set completion, is work.
#[test]
fn an_empty_store_not_completing_the_set_has_no_update_work() {
    let dir = scratch("has-work");
    let layout = crate::store::Layout {
        prefix: dir.join("prefix"),
    };
    std::fs::create_dir_all(layout.bin_dir()).unwrap();
    let none = std::collections::BTreeMap::new();
    let unlinked = std::collections::BTreeSet::new();
    assert!(
        !has_work(&none, &unlinked, false, &layout),
        "nothing to move"
    );
    assert!(
        has_work(&none, &unlinked, true, &layout),
        "the set is being completed"
    );
    let one: std::collections::BTreeMap<String, u64> = [("ay".to_string(), 18)].into();
    assert!(
        has_work(&one, &unlinked, false, &layout),
        "a program is installed"
    );
    let linked: std::collections::BTreeSet<String> = ["ay".to_string()].into();
    assert!(
        has_work(&none, &linked, false, &layout),
        "a development link is the pass's to keep"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A window asks for a near-index hint every 30 seconds. On the usual
/// adopted store, set completion answers yes without walking either the
/// shim tree or every development-link marker; an installed shim also
/// makes the link validation unnecessary. The empty case must still reach
/// the link inventory, because a dev-linked toolset needs update passes.
#[test]
fn near_index_work_check_only_reads_the_inventories_it_needs() {
    assert!(probe_has_work_lazily(
        true,
        || panic!("set completion must skip the shim scan"),
        || panic!("set completion must skip the tombstone scan"),
        || panic!("set completion must skip development-link validation"),
    ));
    assert!(probe_has_work_lazily(
        false,
        || true,
        || panic!("an installed program needs no tombstone scan"),
        || panic!("an installed program needs no development-link validation"),
    ));
    assert!(probe_has_work_lazily(
        false,
        || false,
        || true,
        || panic!("a tombstoned vendor needs no development-link validation"),
    ));
    let linked_read = std::cell::Cell::new(false);
    assert!(probe_has_work_lazily(
        false,
        || false,
        || false,
        || {
            linked_read.set(true);
            true
        },
    ));
    assert!(linked_read.get(), "a dev-only store remains eligible");
}

/// The probe worker outlives a CLI invocation. An adopted but empty store
/// must start looking for newly published members after the user enables
/// Automatic updates in the same running window/session. The old one-time
/// `config::cached()` read kept it ineligible until the process restarted.
#[test]
fn near_index_work_check_reads_live_install_consent() {
    let dir = scratch("has-work-live-config");
    let layout = crate::store::Layout {
        prefix: dir.join("prefix"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let config_path = dir.join("aterm.toml");
    std::fs::write(&config_path, "[packages]\nenabled = false\n").unwrap();
    let mut live = ProbePackagesConfig::new(
        Some(config_path.clone()),
        crate::config::PackagesConfig::default(),
    );
    assert!(!update_pass_has_work_with(&layout, live.poll()));

    std::fs::write(&config_path, "[packages]\nenabled = true\n").unwrap();
    assert!(!update_pass_has_work_with(&layout, live.poll()));

    std::fs::write(layout.adopted(), "").unwrap();
    assert!(update_pass_has_work_with(&layout, live.poll()));
    assert!(update_pass_has_work_with(&layout, live.poll()));

    std::fs::write(&config_path, "[packages]\nenabled = false\n").unwrap();
    assert!(!update_pass_has_work_with(&layout, live.poll()));

    // A malformed table must revoke unattended install eligibility even
    // though the previous readable table allowed it; a repaired table
    // restores the next probe without restarting the worker.
    std::fs::write(&config_path, "[packages]\nenabled = [\n").unwrap();
    assert!(!update_pass_has_work_with(&layout, live.poll()));
    std::fs::write(&config_path, "[packages]\nenabled = true\n").unwrap();
    assert!(update_pass_has_work_with(&layout, live.poll()));

    // A path that exists but cannot be read as a config behaves the same
    // way. The read failure is retried rather than cached as a permanent
    // answer when the path becomes readable again.
    std::fs::remove_file(&config_path).unwrap();
    std::fs::create_dir(&config_path).unwrap();
    assert!(!update_pass_has_work_with(&layout, live.poll()));
    std::fs::remove_dir(&config_path).unwrap();
    std::fs::write(&config_path, "[packages]\nenabled = true\n").unwrap();
    assert!(update_pass_has_work_with(&layout, live.poll()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Read-only surfaces keep working WHILE a mutator holds the store lock — the
/// lock serializes writers only, never observation (`list`/`which`/status/verify
/// must stay live during a long install).
#[test]
fn read_only_paths_need_no_store_lock() {
    let layout = temp_layout("readonly-lock");
    // A concurrent mutator: a second Layout over the SAME prefix holds the lock.
    let holder = crate::store::Layout {
        prefix: layout.prefix.clone(),
    };
    let _held = crate::lock::try_lock_store(&holder).expect("mutator takes the lock");
    // Every read-only surface the lock-free verbs are built on completes normally.
    assert!(crate::list_installed(&layout).is_empty());
    assert!(crate::which(&layout, "ay").is_none());
    assert!(crate::status::read(&layout).is_none());
    assert!(crate::verify::verify_all(&layout).is_empty());
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// REGRESSION: no ambient state may supply the verification anchor.
///
/// `ATPKG_ROOTKEY_OVERRIDE` used to swap which root key anchored verification,
/// and could supply one to a build that had none — an environment variable
/// deciding trust. The anchor is now the committed paper-master keyset and only
/// that, so setting the old variable must change nothing.
#[test]
fn no_environment_variable_can_supply_the_verification_anchor() {
    // NO `set_var` HERE, deliberately. The claim is that the anchor is a
    // COMPILED constant and the variable is not consulted at all — and
    // `effective_anchor` reads no environment (only the store's own durable
    // roster floor), which is the property under test. Mutating the
    // process-global environment to demonstrate that would race every other
    // test in this binary (cargo runs them on threads) to prove something the
    // source already settles, and the lint that flags it is right.
    let layout = temp_layout("anchor");
    assert_eq!(
        effective_anchor(&layout).is_armed(),
        !crate::PKG_TRUST_ANCHORS.is_empty(),
        "the committed keyset is the only thing that arms the manager"
    );
    assert!(
        effective_anchor(&layout).is_armed(),
        "and in THIS tree it is armed (2026-08-15), so the CLI's anchor is live"
    );
    // No env lookup reaches it: aterm-update-core's `tests/env_reads.rs` fails on any
    // `ATPKG_*` read in shipped code that is not on its allow-list.
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The two durable ratchets advance TOGETHER and INDEPENDENTLY: an index build and a
/// roster generation are different counters over different documents, and folding
/// them into one high-water would let either one's advance refuse the other's
/// perfectly current document.
#[test]
fn advance_floors_ratchets_both_counters_separately() {
    let layout = temp_layout("floors");
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
    advance_floors(&layout, 41, 3);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
    // An index republish that does NOT bump the roster leaves the roster floor put,
    // so the generation still in use is not ratcheted out from under the client.
    advance_floors(&layout, 42, 3);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
    // A revocation bumps the roster without re-cutting the index — symmetrically.
    advance_floors(&layout, 42, 4);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 4);
    // And the anchor the CLI builds carries the roster floor it just recorded.
    assert_eq!(effective_anchor(&layout).roster_floor, 4);
    // NON-VACUITY: they are genuinely separate files.
    assert_ne!(layout.floor(), layout.roster_floor());
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A BUILD FLOOR ONE MACHINE DROVE OUT OF REACH IS RE-BASED BY THE NEXT GENERATION.
///
/// `index_build` sits inside MACHINE-signed bytes, so any rostered machine can set it to
/// the u64 ceiling and — under a purely monotonic ratchet — put the store permanently
/// above every index the owner will ever publish, including the one that revokes that
/// machine. Bricked AND unrevocable, by a tier the roster exists to be able to revoke.
///
/// So the floor is scoped to the generation that recorded it: a strictly newer
/// master-signed generation re-bases it. Only the paper master can mint a generation, so
/// this lever is the master's alone.
///
/// MUTATION: replace the `roster_seq > generation.current()` arm in `advance_floors`
/// with the plain `check_and_record` and the rescue assertion fails with the floor still
/// at `u64::MAX`.
#[test]
fn a_newer_generation_rebases_a_floor_a_machine_drove_out_of_reach() {
    let layout = temp_layout("floor-poison");
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();

    // A machine on generation 3 publishes the TOML integer ceiling.
    advance_floors(&layout, u64::MAX, 3);
    assert_eq!(build_floor(&layout).index_build, u64::MAX);
    assert_eq!(build_floor(&layout).roster_seq, 3);
    // PRECONDITION: within that same generation the floor is immovable — this really is
    // the trap, not a floor that would have relaxed on its own.
    advance_floors(&layout, 101, 3);
    assert_eq!(
        build_floor(&layout).index_build,
        u64::MAX,
        "same generation ⇒ monotonic, exactly as before"
    );

    // The owner revokes that machine and republishes at a sane build under generation 4.
    advance_floors(&layout, 101, 4);
    assert_eq!(
        build_floor(&layout),
        crate::sig::BuildFloor {
            index_build: 101,
            roster_seq: 4
        },
        "a newer master-signed generation re-bases the floor it inherited"
    );
    // ...and from there it ratchets again, so the rescue is one step, not a hole.
    advance_floors(&layout, 100, 4);
    assert_eq!(build_floor(&layout).index_build, 101);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

// ---- [packages.links] reconciliation + the auto-install bootstrap ----
// Signed-fixture helpers mirroring flow.rs's (a real USTAR+zstd archive, a
// root-signed index, release-signed pkg manifests) but laid out as a DIR
// REGISTRY so the production `DirFetcher` serves them (§14).

use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::sig::testkit;

/// The synthetic paper master and the one machine it rosters — the SAME fixture the
/// whole crate signs with, so a CLI test cannot accidentally prove something under a
/// trust shape no other layer uses.
const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

/// The anchor the dir-registry tests resolve under: armed with the synthetic master,
/// roster floor 0.
fn test_anchor() -> crate::Anchor {
    crate::Anchor::of(vec![pk(&ROOT_SEED)], 0)
}

/// Publish the master-signed roster beside a `dir:` registry's index. Without it the
/// DirFetcher yields no candidate at all — a registry is index PLUS the generation
/// that authorized its signer, never one without the other.
fn write_roster(dir: &Path) {
    let (bytes, sig) = testkit::published_roster();
    std::fs::write(
        dir.join(aterm_update_core::roster::ROSTER_ASSET),
        bytes.as_slice(),
    )
    .unwrap();
    std::fs::write(
        dir.join(aterm_update_core::roster::ROSTER_SIG_ASSET),
        sig.as_slice(),
    )
    .unwrap();
}

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}
fn pk(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(kp(seed).public_key().as_ref()).expect("32-byte key")
}
fn sign(seed: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    kp(seed).sign(msg).as_ref().to_vec()
}

fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-cli-{label}-"))
}

/// A raw USTAR + zstd archive shipping `bin/<prog>`.
fn make_archive(dir: &Path, prog: &str, build: u64) -> PathBuf {
    fn entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(b"0000755\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + (512 - content.len() % 512) % 512, 0);
        out
    }
    let mut tar = Vec::new();
    tar.extend(entry(&format!("bin/{prog}"), b"#!/bin/true\n"));
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join(format!("{prog}-{build}.tar.zst"));
    let f = std::fs::File::create(&path).unwrap();
    let mut enc = zstd::Encoder::new(f, 0).unwrap();
    enc.write_all(&tar).unwrap();
    enc.finish().unwrap();
    path
}

/// Write one program's release-signed `pkg-<prog>-<build>.toml` (+ `.sig`) beside its
/// real archive, with the archive's genuine sha256 + tree_root baked in.
fn write_pkg(dir: &Path, prog: &str, build: u64) {
    write_pkg_for_triple(dir, prog, build, current_triple());
}

/// [`write_pkg`] with an explicit artifact `target` — a manifest whose one
/// artifact serves a FOREIGN triple is how the registry looks from every
/// machine the publisher does not build for.
fn write_pkg_for_triple(dir: &Path, prog: &str, build: u64, triple: &str) {
    let archive = make_archive(dir, prog, build);
    let sha = crate::tree::file_sha256(&archive).unwrap();
    let probe = dir.join(format!("probe-{prog}"));
    // The extractor's own fold — what a producer signs; on Windows it folds the
    // declared modes the plain walk cannot read (`crate::tree`'s module docs).
    let root = crate::extract::extract_tar_zst_tree(
        &archive,
        &probe,
        10_000_000,
        10_000,
        crate::extract::ExtractOptions::default(),
    )
    .unwrap()
    .root();
    let _ = std::fs::remove_dir_all(&probe);
    let body = format!(
        "schema = 2\nprogram = \"{prog}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
         exposes = [\"{prog}\"]\n\
         [[artifact]]\ntarget = \"{triple}\"\nkind = \"binary\"\n\
         asset = \"{prog}-{build}.tar.zst\"\nsha256 = \"{sha}\"\ntree_root = \"{root}\"\n\
         size = 100\n[artifact.cost]\ndisk_installed = 1048576\n"
    );
    let name = dir.join(format!("pkg-{prog}-{build}.toml"));
    std::fs::write(&name, body.as_bytes()).unwrap();
    std::fs::write(
        dir.join(format!("pkg-{prog}-{build}.toml.sig")),
        sign(&RELEASE_SEED, body.as_bytes()),
    )
    .unwrap();
}

/// Write the root-signed index for a 4-program default set on `channel`:
/// `ay` (installable), `ny` (a second installable, for exclude), `zz`
/// (pin YANKED — must never install), `lk` (installable, dev-linked in
/// the tests) — plus real signed pkgs/archives for the non-yanked members.
fn write_registry(dir: &Path, channel: &str) {
    write_registry_at(dir, channel, 41);
}

/// As [`write_registry`], with an explicit `index_build` (the floor-admission tests
/// need an index strictly above / equal to the durable floor).
fn write_registry_at(dir: &Path, channel: &str, index_build: u64) {
    let index_body = format!(
        "schema = 2\nindex_build = {index_build}\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n[programs.ny]\nrepo = \"ny\"\n\
         [programs.zz]\nrepo = \"zz\"\n[programs.lk]\nrepo = \"lk\"\n\
         [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [\"zz@5\"]\npin = {{ ay = 18, ny = 7, zz = 5, lk = 3 }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
    // The index is MACHINE-signed now; only the roster is signed by the master.
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, index_body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
    write_pkg(dir, "ay", 18);
    write_pkg(dir, "ny", 7);
    write_pkg(dir, "lk", 3);
}

/// A fake checkout with a built `target/release/<prog>` bin.
fn checkout(label: &str, prog: &str) -> crate::testdir::Dir {
    let d = scratch(&format!("co-{label}"));
    std::fs::create_dir_all(d.join("target/release")).unwrap();
    std::fs::write(d.join("target/release").join(prog), b"#!/bin/true\n").unwrap();
    d
}

/// A COUNTING [`crate::DirFetcher`]. Every index resolve reaches the fetcher through
/// `index_candidates`, which makes the counter the instrument for "the pass
/// verify-selects ONCE": without it, a lane that quietly resolved a second time would
/// still pass every functional assertion.
struct CountingFetcher {
    inner: crate::DirFetcher,
    resolves: std::cell::Cell<u32>,
    fail_first_index: std::cell::Cell<bool>,
    first_index_link_down: bool,
}

impl CountingFetcher {
    fn new(dir: PathBuf) -> Self {
        Self {
            inner: crate::DirFetcher::new(dir),
            resolves: std::cell::Cell::new(0),
            fail_first_index: std::cell::Cell::new(false),
            first_index_link_down: false,
        }
    }

    fn resolves(&self) -> u32 {
        self.resolves.get()
    }
}

impl crate::flow::Fetcher for CountingFetcher {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        self.resolves.set(self.resolves.get() + 1);
        if self.fail_first_index.replace(false) {
            return Err("transient first index listing failure".into());
        }
        crate::flow::Fetcher::index_candidates(&self.inner)
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        crate::flow::Fetcher::pkg_manifest(&self.inner, repo, program, build)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        crate::flow::Fetcher::download(&self.inner, repo, asset, dest)
    }

    fn source_id(&self) -> String {
        crate::flow::Fetcher::source_id(&self.inner)
    }

    fn index_link_down(&self) -> bool {
        self.first_index_link_down
    }
}

/// An empty-store update can retry a transiently failed initial resolve in
/// set completion. The pass-end witness must name that retry's signed build,
/// even when the requested hint named an older build and nothing installed.
#[test]
fn empty_store_retry_records_the_signed_index_it_resolved() {
    let dir = scratch("retry-witness-registry");
    write_registry_at(&dir, "stable", 46);
    let layout = temp_layout("retry-witness-store");
    let fetcher = CountingFetcher::new(dir.clone());
    fetcher.fail_first_index.set(true);
    let cfg = crate::config::PackagesConfig {
        exclude: Some(
            ["ay", "ny", "zz", "lk", "claude", "codex"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let seams = PassSeams {
        anchor: |_| test_anchor(),
        now: || 0,
        trust: &trust,
        shell_hooks: no_shell_hooks,
        rustup_seam: &no_rustup_seam,
        progress: None,
        path: Some(std::ffi::OsString::new()),
        warm_agents: record_warm,
        translated: || false,
        flip_policy: || crate::quiet::FlipPolicy::Now,
        running: &no_processes,
    };
    PUBLISHED_INDEX_HINT.with(|hint| hint.set(Some(45)));
    let _ = run_recorded_update_pass(
        &layout,
        &cfg,
        &fetcher,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    PUBLISHED_INDEX_HINT.with(|hint| hint.set(None));
    assert!(
        fetcher.resolves() >= 2,
        "the initial resolve and default-set retry both reached the fetcher"
    );
    let status = crate::status::read(&layout).expect("pass-end record");
    assert!(
        !status.programs.contains_key("*index*"),
        "the default-set retry resolved the index before empty-store diagnosis"
    );
    assert_eq!(status.last_pass_attempted_index_build, 46);
    assert_eq!(status.last_pass_attempted_at, status.last_pass_at);
    let stamps = aterm_update_core::pkg_check::Stamps::read(&layout.status());
    assert!(
        !stamps.completed_older_index_pass(46),
        "a second pass for the signed build must keep the spacing hold"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

fn assert_empty_store_retry_success(first_index_link_down: bool) {
    // The two variants run concurrently under libtest. Distinct fixture paths
    // keep one pass's stage and GC from removing the other's live files.
    let kind = if first_index_link_down {
        "link-down"
    } else {
        "refusal"
    };
    let dir = scratch(&format!("retry-provenance-registry-{kind}"));
    write_registry_at(&dir, "stable", 46);
    let layout = temp_layout(&format!("retry-provenance-store-{kind}"));
    let mut fetcher = CountingFetcher::new(dir.clone());
    fetcher.fail_first_index.set(true);
    fetcher.first_index_link_down = first_index_link_down;
    // Leave one signed member installable so this is a completed pass, not
    // the intentional exit-2 verdict for an empty, wholly excluded toolset.
    let cfg = crate::config::PackagesConfig {
        exclude: Some(
            ["ny", "zz", "lk", "claude", "codex"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let seams = PassSeams {
        anchor: |_| test_anchor(),
        // A second past the epoch, not 0: the pass stamps its success at its own clock,
        // and 0 is the "clock unusable" sentinel that stamps nothing (`rfc3339_at`).
        now: || 1,
        trust: &trust,
        shell_hooks: no_shell_hooks,
        rustup_seam: &no_rustup_seam,
        progress: None,
        path: Some(std::ffi::OsString::new()),
        warm_agents: record_warm,
        translated: || false,
        flip_policy: || crate::quiet::FlipPolicy::Now,
        running: &no_processes,
    };
    let code = run_recorded_update_pass(
        &layout,
        &cfg,
        &fetcher,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert!(fetcher.resolves() >= 2, "the default-set lane retried");
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(18),
        "the signed retry installed the missing member"
    );
    assert_eq!(
        code, 0,
        "a successful retry is not the first resolve's verdict"
    );
    let status = crate::status::read(&layout).expect("completed pass record");
    assert_eq!(status.last_pass, "ok");
    assert!(!status.last_success_at.is_empty());
    assert!(!status.programs.contains_key("*index*"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_store_retry_after_refusal_reports_success() {
    assert_empty_store_retry_success(false);
}

#[test]
fn empty_store_retry_after_link_failure_reports_success() {
    assert_empty_store_retry_success(true);
}

/// A ROSETTA PASS WRITES ITS OWN ROW. Every full pass records its end, so the window
/// takes a `*toolset*` row in a record the pass moved for that pass's — and the
/// translated refusal (exit 2) left an earlier native offline pass's "index
/// unreachable" row standing, which the window read as this pass's: the failure ladder
/// instead of settling. The refusal now writes its own verdict over it.
#[test]
fn a_translated_pass_replaces_an_earlier_passs_unreachable_row() {
    let dir = scratch("translated-registry");
    write_registry_at(&dir, "stable", 46);
    let layout = temp_layout("translated-store");
    record_status(
        &layout,
        "*toolset*",
        crate::ProgramStatus {
            installed_build: None,
            state: String::from(crate::state::TOOLSET_INDEX_UNREACHABLE),
            tree_root: String::new(),
        },
        "an earlier native pass found the index unreachable".to_string(),
    );
    let cfg = crate::config::PackagesConfig {
        exclude: Some(
            ["ay", "ny", "zz", "lk", "claude", "codex"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let seams = PassSeams {
        anchor: |_| test_anchor(),
        // A second past the epoch, not 0: the pass stamps its success at its own clock,
        // and 0 is the "clock unusable" sentinel that stamps nothing (`rfc3339_at`).
        now: || 1,
        trust: &trust,
        shell_hooks: no_shell_hooks,
        rustup_seam: &no_rustup_seam,
        progress: None,
        path: Some(std::ffi::OsString::new()),
        warm_agents: record_warm,
        translated: || true,
        flip_policy: || crate::quiet::FlipPolicy::Now,
        running: &no_processes,
    };
    let code = run_recorded_update_pass(
        &layout,
        &cfg,
        &crate::DirFetcher::new(dir.clone()),
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 2, "the translated refusal");
    let status = crate::status::read(&layout).expect("pass-end record");
    // The pass reached the signed index (the registry is the channel here), so exit 2
    // is a verdict no retry changes and the record says a check ran
    // (`finish_update_pass`, the derived model `AtpkgPassStamps`).
    assert_eq!(status.last_pass, "ok");
    assert!(!status.last_success_at.is_empty());
    assert_eq!(
        status.programs["*toolset*"].state,
        crate::state::TOOLSET_TRANSLATED,
        "the row is this pass's verdict, not the earlier pass's"
    );
    assert!(
        status.outcome.contains("Rosetta translation"),
        "{}",
        status.outcome
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE PASS, ONE VERIFY-SELECT. `apply_channel` resolved and verified the signed
/// index, and then every other lane of the SAME pass resolved it again: the
/// set-completion lane an adopted machine runs every tick, the attestation repair,
/// the alias reconcile (through the §14 cache), the empty-store diagnosis. Each
/// repeat re-read and TOML-parsed `index-cache.toml`, re-ran the master + machine
/// signature checks over every candidate, re-admitted the roster and re-observed its
/// generation — for an answer that cannot differ inside one pass, and not one byte of
/// it network work. The pass resolves once and hands that index down.
///
/// The attestation repair reads the REAL clock, so a fixture index whose signed
/// window is long past cannot exercise it here.
#[test]
fn a_pass_verify_selects_the_index_once_and_hands_it_to_every_lane() {
    let dir = scratch("one-resolve");
    write_registry(&dir, "stable");
    let layout = temp_layout("one-resolve");
    let fetcher = CountingFetcher::new(dir.clone());
    // `ny` and `zz` are narrowed out, and both measured passes below run over a
    // COMPLETE set: what is under test is the LANE's own verify-select, and a lane
    // that still has members to install also runs the per-program `flow::install`
    // path, which resolves for itself (the loop `apply_channel`'s own doc contrasts
    // itself with) and would drown the number being measured.
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ny".into(), "zz".into()]),
        ..Default::default()
    };
    // The pass's one verify-select, as the apply lane runs it.
    let index =
        crate::resolve_verified_index(&fetcher, &layout, &test_anchor(), build_floor(&layout), 0)
            .expect("the signed fixture index verifies");
    assert_eq!(fetcher.resolves(), 1, "the pass resolved its index");

    // Lay the set down first (this pass installs, so it resolves per program too).
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.index_error, None);
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(18),
        "a real pass: the missing members installed"
    );
    let settled = fetcher.resolves();

    // THE FINDING, on the shape the six-hourly tick actually meets: a complete set,
    // nothing to install. Handed the pass's index, the set-completion lane
    // verify-selects NOTHING.
    let out = super::install_default_set_with_path(
        &layout,
        &fetcher,
        &test_anchor(),
        &cfg,
        super::Consent::InstallingAterm,
        0,
        Some(&index),
        Some(std::ffi::OsStr::new("")),
        &crate::quiet::FlipGate::NOW,
        &std::collections::BTreeSet::new(),
    );
    assert_eq!(
        out.index_error, None,
        "the handed index IS the pass's index"
    );
    assert_eq!(
        fetcher.resolves(),
        settled,
        "the set-completion lane re-used the pass's index"
    );

    // …while the same pass with no index in hand resolves one for itself: that second
    // verify-select per tick is exactly what the update pass used to pay.
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.index_error, None);
    assert_eq!(
        fetcher.resolves(),
        settled + 1,
        "a lane holding no index resolves one"
    );
    let settled = fetcher.resolves();

    // AND NO GATE IS SKIPPED. Past the signed `valid_until` the held index is refused
    // re-use, the lane resolves for itself, and that resolve meets the identical
    // freshness refusal it always did.
    let lapsed = 4_000_000_000;
    assert!(
        reusable_index(Some(&index), lapsed).is_none(),
        "a lapsed freshness window is never re-used"
    );
    let out = super::install_default_set_with_path(
        &layout,
        &fetcher,
        &test_anchor(),
        &cfg,
        super::Consent::InstallingAterm,
        lapsed,
        Some(&index),
        Some(std::ffi::OsStr::new("")),
        &crate::quiet::FlipGate::NOW,
        &std::collections::BTreeSet::new(),
    );
    assert!(
        out.index_error.is_some(),
        "a lapsed index is refused, not quietly re-used"
    );
    assert_eq!(
        fetcher.resolves(),
        settled + 1,
        "the lapsed index fell through to a real resolve, which refused it"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn update_counts_live_development_toolsets_and_records_broken_links() {
    use std::os::unix::fs::PermissionsExt as _;
    let layout = temp_layout("update-linked-inventory");
    let co = checkout("update-linked-inventory", "ay");
    let binary = co.join("target/release/ay");
    let empty = std::collections::BTreeMap::new();
    assert!(update_toolset_is_empty(
        &empty,
        &update_link_inventory(&layout).unwrap()
    ));
    crate::link(&layout, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    // A real marker and regular file without execute permission are not a
    // provisioned toolset. The failure is durable and cannot mint a success.
    assert!(update_link_inventory(&layout).is_err());
    let failed = crate::status::read(&layout).unwrap();
    assert!(failed.programs["*links*"].state.contains("non-executable"));
    assert!(failed.last_success_at.is_empty());
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let linked = update_link_inventory(&layout).unwrap();
    assert_eq!(
        linked.iter().map(String::as_str).collect::<Vec<_>>(),
        ["ay"]
    );
    assert!(
        crate::active_builds(&layout).is_empty(),
        "no invented registry build"
    );
    assert!(
        !update_toolset_is_empty(&crate::active_builds(&layout), &linked),
        "regression: a dev-link-only toolset is not empty/unsupported"
    );
    assert!(
        !crate::status::read(&layout)
            .unwrap()
            .programs
            .contains_key("*links*"),
        "repair retires exactly the fault it disproves"
    );
    std::fs::remove_file(&binary).unwrap();
    assert!(
        update_link_inventory(&layout).is_err(),
        "deleted checkout must fail again"
    );
    assert!(
        crate::is_linked(&layout, "ay"),
        "validation must not remove update protection"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&co);
}

/// The newer pass separates a present local toolset from a network check. A
/// dev-link-only pass succeeds without inventing a registry build or check,
/// and a checkout disappearing before the final inventory still fails.
#[test]
#[cfg(unix)]
fn a_dev_link_only_update_pass_preserves_tools_without_fabricating_an_index_check() {
    use crate::vendor_direct::lane::world::{self, Fake};
    use std::os::unix::fs::PermissionsExt as _;
    let layout = temp_layout("update-linked-pass");
    let co = checkout("update-linked-pass", "ay");
    let binary = co.join("target/release/ay");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::link(&layout, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    let fetcher = Fake::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let cfg = crate::config::PackagesConfig::default();
    let installed = crate::active_builds(&layout);
    assert!(installed.is_empty(), "a dev link has no registry build");
    assert_eq!(
        run_update_pass(&layout, &cfg, &fetcher, &installed, false, &seams),
        0
    );
    let status = crate::status::read(&layout).unwrap();
    assert!(
        status
            .outcome
            .contains("1 dev-linked program(s) kept, not updated")
    );
    assert!(
        status.last_success_at.is_empty(),
        "no network check was made"
    );
    assert!(!status.programs.contains_key("*toolset*"));
    assert_eq!(fetcher.index_reads.get(), 0);
    assert!(binary.is_file());
    assert!(crate::is_linked(&layout, "ay"));

    std::fs::remove_file(&binary).unwrap();
    assert_eq!(
        run_update_pass(&layout, &cfg, &fetcher, &installed, false, &seams),
        1
    );
    let status = crate::status::read(&layout).unwrap();
    assert!(status.programs["*links*"].state.contains("missing"));
    assert!(status.last_success_at.is_empty());
    assert!(crate::is_linked(&layout, "ay"), "protection is preserved");
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&co);
}

// THE auto-install decision matrix (§11), against the production DirFetcher over a
// real signed dir registry: a missing default-set member INSTALLS; an `exclude`d
// member is narrowed out; a dev-linked member hard-skips; a yanked pin never
// installs; an already-installed member is left to the update pass. None of the
// skips count as failures (the 6h loop must not scream about correct states).
#[test]
fn default_set_bootstrap_installs_missing_and_skips_excluded_linked_yanked() {
    let dir = scratch("bootstrap");
    write_registry(&dir, "stable");
    let layout = temp_layout("bootstrap");
    // Dev-link `lk` (the hard-skip member).
    let co = checkout("bootstrap", "lk");
    crate::link(&layout, "lk", &co, &[PathBuf::from("target/release/lk")]).unwrap();
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ny".into()]),
        ..Default::default()
    };
    let fetcher = crate::DirFetcher::new(dir.clone());
    let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
    assert_eq!(failures, 0, "skips are never failures");
    let active = crate::active_builds(&layout);
    assert_eq!(
        active.get("ay").copied(),
        Some(18),
        "missing member installed"
    );
    assert!(!active.contains_key("ny"), "excluded member narrowed out");
    assert!(!active.contains_key("zz"), "yanked pin never installs");
    assert!(!active.contains_key("lk"), "dev-linked member hard-skipped");
    assert!(
        crate::is_linked(&layout, "lk"),
        "the dev link survives untouched"
    );
    // Idempotence: a second pass finds ay installed and re-installs nothing.
    let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
    assert_eq!(failures, 0);
    assert_eq!(crate::active_builds(&layout).get("ay").copied(), Some(18));
    // The durable floor advanced to the trusted index (§8 gate 3).
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&co);
}

// THE YANK LIST IS HONOURED BY EVERY LANE, WITH ONE STATE STRING. `zz` is pinned at
// 5 and `yanked = ["zz@5"]`: the gate decides Tombstone; the default-set pass skips
// it (no failure, no store dir, no shim) and records `tombstoned: pin yanked/below
// floor`; the update lane tombstones a LIVE zz@5 with the same row and takes its
// shims off PATH. Supply side will yank trust-mc 20065 — this is what a client does
// with that.
#[test]
fn a_yanked_pinned_build_is_skipped_with_a_recorded_tombstone_row() {
    let dir = scratch("yank-row");
    write_registry(&dir, "stable");
    let layout = temp_layout("yank-row");
    let fetcher = crate::DirFetcher::new(dir.clone());
    // The pure gate, first.
    let index =
        crate::resolve_verified_index(&fetcher, &layout, &test_anchor(), build_floor(&layout), 0)
            .expect("the signed fixture index verifies");
    let ch = index
        .channels
        .iter()
        .find(|c| c.name == "stable")
        .expect("the stable channel");
    assert!(crate::gate::is_yanked(ch, "zz", 5));
    assert_eq!(
        crate::gate::decide(ch, "zz", None),
        crate::ApplyDecision::Tombstone
    );
    assert_eq!(
        crate::gate::decide(ch, "zz", Some(5)),
        crate::ApplyDecision::Tombstone,
        "a live copy of the yanked pin is not kept either"
    );
    // The default-set lane: skipped, said, recorded.
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ny".into()]),
        ..Default::default()
    };
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.failures, 0, "a yanked pin is a skip, never a failure");
    let active = crate::active_builds(&layout);
    assert!(!active.contains_key("zz"), "the yanked pin never installs");
    assert!(
        !layout.build_dir("zz", 5).exists(),
        "not a byte of the yanked build is staged"
    );
    assert!(crate::which(&layout, "zz").is_none(), "no shim promises it");
    let status = crate::status::read(&layout).expect("a status record");
    assert_eq!(status.programs["zz"].state, crate::state::TOMBSTONED_PIN);
    assert_eq!(status.programs["zz"].installed_build, None);
    assert!(
        crate::state::TOMBSTONED_PIN.starts_with("tombstoned:"),
        "doctor reads the row as a fault by this prefix"
    );
    // The update lane over a LIVE copy of the yanked build: tombstoned with the
    // same words, shims off PATH.
    make_live(&layout, "zz", 5);
    assert_eq!(crate::active_builds(&layout).get("zz").copied(), Some(5));
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    assert!(
        matches!(outcome_of(&report, "zz"), crate::TxnOutcome::Tombstoned(_)),
        "{:?}",
        report.groups
    );
    let status = crate::status::read(&layout).expect("a status record");
    assert_eq!(status.programs["zz"].state, crate::state::TOMBSTONED_PIN);
    assert!(
        !crate::active_builds(&layout).contains_key("zz"),
        "a tombstoned program's shims no longer resolve to a build"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN EMPTY STORE'S VERDICT NAMES ITS REAL CAUSE. `seed_serviceable` is empty when the
/// index publishes nothing for this triple AND when the user's own `[packages].exclude`
/// or uninstalls took everything it does publish — and reading both as "no build for
/// this machine's architecture" told a user their Mac was unserved when their config
/// was the cause (review of Phase 5, 2026-09-23). `index_serves` asks the question
/// again without the narrowing, and only an empty unnarrowed set blames the CPU.
#[test]
fn an_empty_store_blames_the_architecture_only_when_nothing_is_published_for_it() {
    let dir = scratch("index-serves");
    write_registry(&dir, "stable");
    // Every member publishes for this triple — `zz` too: its pin is yanked, but the
    // prescan asks about artifacts, and a member with no manifest at all would be
    // counted as served (an unverifiable manifest is skipped, not missing).
    write_pkg(&dir, "zz", 5);
    let layout = temp_layout("index-serves");
    let fetcher = crate::DirFetcher::new(dir.clone());
    let resolve = |fetcher: &crate::DirFetcher| {
        crate::resolve_verified_index(fetcher, &layout, &test_anchor(), build_floor(&layout), 0)
            .expect("the signed fixture index verifies")
    };
    let index = resolve(&fetcher);
    let exclude = |names: &[&str]| crate::config::PackagesConfig {
        exclude: Some(names.iter().map(|n| (*n).to_string()).collect()),
        ..Default::default()
    };
    let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    // Served.
    let plain = crate::config::PackagesConfig::default();
    assert_eq!(
        index_serves(&layout, &fetcher, &index, &plain),
        IndexServes::Yes
    );
    // `[packages].exclude` names every served member: the exclusion, not the CPU.
    let all_out = exclude(&["ay", "ny", "lk", "zz"]);
    assert_eq!(
        index_serves(&layout, &fetcher, &index, &all_out),
        IndexServes::Narrowed(Narrowing {
            excluded: names(&["ay", "lk", "ny", "zz"]),
            removed: Vec::new(),
        })
    );
    // Uninstalled one by one: the removals, and read as a decision.
    std::fs::write(layout.removed(), "ay\nny\nlk\nzz\n").unwrap();
    let removed = index_serves(&layout, &fetcher, &index, &plain);
    assert_eq!(
        removed,
        IndexServes::Narrowed(Narrowing {
            excluded: Vec::new(),
            removed: names(&["ay", "lk", "ny", "zz"]),
        })
    );
    let IndexServes::Narrowed(narrowing) = removed else {
        unreachable!()
    };
    let (said, state) = narrowing.verdict(current_triple());
    assert!(seed_unusable_is_informational(&said), "{said}");
    assert_eq!(state, crate::state::TOOLSET_REMOVED);
    assert!(!crate::doctor::is_recorded_problem("*toolset*", state));
    // Both at once: each member named under its own cause, and the exclusion's
    // sentence — a failure a person can act on — is the one said.
    std::fs::write(layout.removed(), "ny\nlk\nzz\n").unwrap();
    let mixed = index_serves(&layout, &fetcher, &index, &exclude(&["ay"]));
    let IndexServes::Narrowed(narrowing) = mixed else {
        panic!("{mixed:?}")
    };
    assert_eq!(narrowing.excluded, names(&["ay"]));
    assert_eq!(narrowing.removed, names(&["lk", "ny", "zz"]));
    let (said, state) = narrowing.verdict(current_triple());
    assert!(!seed_unusable_is_informational(&said), "{said}");
    assert_eq!(state, crate::state::TOOLSET_EXCLUDED);
    assert!(crate::doctor::is_recorded_problem("*toolset*", state));
    // NEGATIVE CONTROL: when nothing is published for this triple the architecture
    // IS the answer, exclusions and removals notwithstanding.
    for (prog, build) in [("ay", 18), ("ny", 7), ("lk", 3), ("zz", 5)] {
        write_pkg_for_triple(&dir, prog, build, "riscv64gc-unknown-linux-gnu");
    }
    let index = resolve(&fetcher);
    for cfg in [&plain, &all_out] {
        assert_eq!(
            index_serves(&layout, &fetcher, &index, cfg),
            IndexServes::Unserved(names(&["riscv64gc-unknown-linux-gnu"])),
            "the verdict names the machine ALab does build for"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AUDIT-2 ITEM 2 (+ design §2): a program the registry publishes NO artifact for
/// on this triple neither announces, nor counts as a failure, nor leaves a pending
/// stub on PATH — and ITS OWN status row records the canonical `unavailable on
/// <target>: <hint>` state, so a pre-existing stub's `__pending` tells the truth
/// instead of promising a launch will provision it. Served members in the same pass
/// still install normally, and BECAUSE one does, the toolset-wide `blocked` verdict
/// (reserved for a machine the index serves nothing on) is NOT raised: a partial
/// matrix is a normal state, never a fault `doctor` reports.
#[test]
fn unserved_triple_members_skip_quietly_with_a_recorded_reason() {
    let dir = scratch("unserved");
    write_registry(&dir, "stable");
    // ay's one artifact serves a machine this test is not running on.
    write_pkg_for_triple(&dir, "ay", 18, "riscv64gc-unknown-linux-gnu");
    let layout = temp_layout("unserved");
    let cfg = crate::config::PackagesConfig::default();
    let fetcher = crate::DirFetcher::new(dir.clone());
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(
        out.failures, 0,
        "unserved is a correct state, not a failure"
    );
    let active = crate::active_builds(&layout);
    assert!(!active.contains_key("ay"), "no artifact, no install");
    assert_eq!(
        active.get("ny").copied(),
        Some(7),
        "served members still install in the same pass"
    );
    assert!(
        !crate::stub::pending_stub_exists(&layout, "ay"),
        "no stub for a program whose bytes can never move here"
    );
    let status = crate::status::read(&layout).expect("status recorded");
    let row = status.programs.get("ay").expect("ay's own row");
    assert_eq!(row.state, crate::state::unavailable(current_triple(), ""));
    assert!(
        !status.programs.contains_key("*toolset*"),
        "ny was served, so the machine is not blocked as a whole: {:?}",
        status.programs.keys().collect::<Vec<_>>()
    );
    // (`zz`'s yanked pin IS a fault — its `tombstoned:` row is the point of
    // `a_yanked_pinned_build_is_skipped_with_a_recorded_tombstone_row`.)
    assert!(
        crate::doctor::recorded_problems(Some(&status))
            .iter()
            .all(|p| !p.starts_with("ay:")),
        "an unserved member is a state, never a doctor fault"
    );
    let lines = pending_state_lines(&layout, "ay");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&crate::state::unavailable(current_triple(), ""))),
        "__pending surfaces the recorded reason, not the launch promise: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A two-program registry: `ay` and `xc` (`extra = true`, a key an older index carries
/// and this client ignores), `xc`'s one artifact serving `extra_triple`.
fn write_registry_with_extra(dir: &Path, channel: &str, extra_triple: &str) {
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n[programs.xc]\nrepo = \"xc\"\nextra = true\n\
         [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ ay = 18, xc = 9 }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, index_body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
    write_pkg(dir, "ay", 18);
    write_pkg_for_triple(dir, "xc", 9, extra_triple);
}

/// A three-program registry for the retired-`requires` tests: `ay` (`requires =
/// ["xc"]`), `ny` (no requires), `xc` (`extra = true`, a key an older index carries and
/// this client ignores) — every pkg served for this triple.
fn write_registry_with_requires(dir: &Path) {
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\nrequires = [\"xc\"]\n\
         [programs.ny]\nrepo = \"ny\"\n\
         [programs.xc]\nrepo = \"xc\"\nextra = true\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ ay = 18, ny = 7, xc = 9 }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, index_body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
    write_pkg(dir, "ay", 18);
    write_pkg(dir, "ny", 7);
    write_pkg(dir, "xc", 9);
}

/// THE RETIRED `requires` THROUGH THE SET-COMPLETION PASS (§5.3(b)/(c)): `ay`'s
/// signed row names it, so the pass refuses ay — an `error:` row naming the refusal,
/// no byte downloaded, no artifact resolved — while `ny` and the former extra `xc`,
/// which name nothing, install beside it as default-set members.
#[cfg(unix)]
#[test]
fn the_pass_refuses_a_program_naming_the_retired_requires_and_installs_the_rest() {
    let dir = scratch("requires-retired");
    write_registry_with_requires(&dir);
    let layout = temp_layout("requires-retired");
    let cfg = crate::config::PackagesConfig::default();
    let fetcher = crate::DirFetcher::new(dir.clone());
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.failures, 1, "the refusal is named, never silent");
    let active = crate::active_builds(&layout);
    assert!(!active.contains_key("ay"), "refused");
    assert_eq!(
        active.get("ny").copied(),
        Some(7),
        "the rest of the set installs"
    );
    assert_eq!(
        active.get("xc").copied(),
        Some(9),
        "an older index's extra is a default-set member"
    );
    assert!(
        !layout.staging_dir("ay").exists(),
        "refused before any byte moved"
    );
    assert!(!out.resolved_assets.contains_key("ay"));
    let status = crate::status::read(&layout).expect("recorded");
    assert_eq!(
        status.programs["ay"].state,
        format!("error: {}", crate::FlowError::RequiresRetired("ay".into()))
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One update pass over `layout`, exactly as `cmd_update_all` runs it — the channel
/// apply over the live builds, then the per-group report that writes the rows —
/// returning the report and its failure count.
fn update_pass(
    layout: &crate::store::Layout,
    fetcher: &crate::DirFetcher,
) -> (crate::ChannelApplyReport, u32) {
    let installed = crate::active_builds(layout);
    let report = crate::apply_channel(
        fetcher,
        layout,
        &test_anchor(),
        "stable",
        current_triple(),
        &installed,
        &[],
        build_floor(layout),
        0,
    )
    .expect("the channel applies");
    let failures = report_channel_apply(layout, &installed, &report, &[], true);
    (report, failures)
}

/// The outcome the report carries for `program`'s group.
fn outcome_of<'a>(report: &'a crate::ChannelApplyReport, program: &str) -> &'a crate::TxnOutcome {
    &report
        .groups
        .iter()
        .find(|(g, _)| g.members.iter().any(|m| m == program))
        .unwrap_or_else(|| panic!("{program}'s group is in the report: {:?}", report.groups))
        .1
}

// An UpToDate pass rewrites every live member's row to name THIS pass's index —
// migrating the legacy `active` spelling and re-stamping `pinned by index 15` to the
// current build — while keeping the pass's own outcome sentence, and leaving every
// other spelling (a SHADOWED row, a fault) to the arm that owns it.
#[test]
fn an_up_to_date_pass_migrates_legacy_rows_and_restamps_the_index() {
    assert!(up_to_date_row_needs_rewrite(None, 18, 41));
    assert!(up_to_date_row_needs_rewrite(Some("active"), 18, 41));
    assert!(up_to_date_row_needs_rewrite(
        Some(&crate::state::managed(18, 15)),
        18,
        41
    ));
    assert!(!up_to_date_row_needs_rewrite(
        Some(&crate::state::managed(18, 41)),
        18,
        41
    ));
    for other in [
        crate::state::shadowed(18, Path::new("/opt/homebrew/bin/ay")),
        "tombstoned: pin yanked/below floor".to_string(),
        crate::state::system(Path::new("/p"), None),
    ] {
        assert!(
            !up_to_date_row_needs_rewrite(Some(&other), 18, 41),
            "{other} belongs to another arm"
        );
    }
    // THE FAULT LIFT (observed live 2026-09-14): a row a failed pass wrote is the
    // one thing no later pass took back, so `clean`/`nn`/`ny` read `aborted: stage`
    // for days beside the healthy builds they were running and doctor said PROBLEM.
    // A member this pass finds live at its pin has, by that fact, no such fault.
    for fault in [
        "aborted: stage",
        "aborted: flip (rolled back)",
        "error: coherence group 'trust' bootstrap aborted",
        // An older client's requirement-gate row (retired 2026-09-24, §5.3(b)).
        "blocked by xc: not installed",
    ] {
        assert!(
            up_to_date_row_needs_rewrite(Some(fault), 18, 41),
            "{fault} is over — the UpToDate pass lifts it"
        );
    }
    let dir = scratch("update-restamp");
    write_registry(&dir, "stable"); // index_build 41, ay pinned at 18
    let layout = temp_layout("update-restamp");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_live(&layout, "ay", 18);
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: "active".into(),
            tree_root: "abc".into(),
        },
        "up to date (index build 41)".into(),
    );
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    assert!(matches!(
        outcome_of(&report, "ay"),
        crate::TxnOutcome::UpToDate
    ));
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ay"].state, crate::state::managed(18, 41));
    assert_eq!(status.programs["ay"].installed_build, Some(18));
    assert_eq!(
        status.programs["ay"].tree_root, "abc",
        "the attestation survives a row rewrite"
    );
    assert_eq!(
        status.outcome, "up to date (index build 41)",
        "a row migration never speaks for the pass"
    );
    // A stale index stamp is refreshed the same way.
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(18),
            state: crate::state::managed(18, 15),
            tree_root: "abc".into(),
        },
        "up to date (index build 41)".into(),
    );
    let _ = update_pass(&layout, &fetcher);
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ay"].state, crate::state::managed(18, 41));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program the person PINNED is held by the pass, and its row says so in their
/// words — who held it and the one command that lets it move — never a fault.
#[test]
fn a_pinned_program_is_held_in_the_persons_words() {
    let dir = scratch("update-pinned");
    write_registry(&dir, "stable"); // ay pinned at 18
    let layout = temp_layout("update-pinned");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_live(&layout, "ay", 17);
    crate::pin::set_pinned(&layout, "ay", true).unwrap();
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0, "a hold is no failure");
    assert!(matches!(
        outcome_of(&report, "ay"),
        crate::TxnOutcome::Pinned(_)
    ));
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(
        status.programs["ay"].state,
        "held \u{2014} you pinned it (`aterm pkg unpin ay` lets it update)"
    );
    assert_eq!(status.programs["ay"].installed_build, Some(17));
    assert!(
        crate::doctor::recorded_problems(Some(&status)).is_empty(),
        "the doctor reads a hold as no problem"
    );
    assert_eq!(crate::active_builds(&layout).get("ay"), Some(&17));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE UPDATE LANE REFUSES THE RETIRED `requires` (§5.3(b)): `ay` (installed at 17,
/// pinned at 18) names it, so the pass aborts ay's group with the refusal's sentence —
/// held on 17, not a byte staged, no artifact resolved — and counts it: a member this
/// client will not move is a failure the pass names, never a silent hold.
#[test]
fn the_update_pass_refuses_an_installed_program_naming_the_retired_requires() {
    let dir = scratch("update-requires-retired");
    write_registry_with_requires(&dir);
    let layout = temp_layout("update-requires-retired");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_live(&layout, "ay", 17);
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 1);
    assert_eq!(
        *outcome_of(&report, "ay"),
        crate::TxnOutcome::Aborted {
            failed: "ay".into(),
            during_flip: false,
            why: crate::FlowError::RequiresRetired("ay".into()).to_string(),
        }
    );
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(17),
        "held on its current build"
    );
    assert!(
        !layout.build_dir("ay", 18).exists() && !layout.staging_dir("ay").exists(),
        "not a byte staged"
    );
    assert!(!report.resolved_assets.contains_key("ay"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An installed `rustc`-locked tuple's registry: the channel moves `ta`@3 + `tb`@5 to
/// `ta`@4 + `tb`@6, with tb's new build published only for `tb_triple` (how it looks from a
/// host the publisher does not build that pin for), and `yanked` as the deny-list.
fn write_installed_group_registry(dir: &Path, tb_triple: &str, yanked: &[&str]) {
    write_group_index(dir, "ta = 4, tb = 6", yanked);
    write_pkg(dir, "ta", 4);
    write_pkg_for_triple(dir, "tb", 6, tb_triple);
}

/// The signed index (build 51) of a `rustc`-locked tuple `ta` + `tb` pinned at `pins`,
/// with `yanked` as the channel's deny-list, published beside its roster.
fn write_group_index(dir: &Path, pins: &str, yanked: &[&str]) {
    let yanked: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
    let index_body = format!(
        "schema = 2\nindex_build = 51\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ta]\nrepo = \"ta\"\ncoherence_group = \"rustc\"\n\
         [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         yanked = [{yanked}]\npin = {{ {pins} }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ,
        yanked = yanked.join(", ")
    );
    std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, index_body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
}

/// `ta`@3 + `tb`@5 genuinely live, each with a managed row and a recorded root.
fn make_installed_group_live(layout: &crate::store::Layout) {
    for (p, b) in [("ta", 3u64), ("tb", 5u64)] {
        make_live(layout, p, b);
        record_status(
            layout,
            p,
            crate::ProgramStatus {
                installed_build: Some(b),
                state: crate::state::managed(b, 50),
                tree_root: format!("root{b}"),
            },
            "up to date (index build 50)".into(),
        );
    }
}

/// A tuple that cannot fully exist on this host is a correct state, not a failure
/// ([`crate::flow::group_missing_triple`]): the group is held, no failure is counted, each
/// installed member's row says which build is unpublished and which one stays, the live
/// links do not move, and nothing is staged. The pass after the build publishes moves it.
#[test]
fn an_installed_group_whose_new_pin_is_unpublished_here_is_held_not_failed() {
    let dir = scratch("update-held-triple");
    write_installed_group_registry(&dir, "riscv64gc-unknown-linux-gnu", &[]);
    let layout = temp_layout("update-held-triple");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_installed_group_live(&layout);
    let links = |layout: &crate::store::Layout| {
        [
            std::fs::read_link(layout.program_current("ta")).unwrap(),
            std::fs::read_link(layout.program_current("tb")).unwrap(),
        ]
    };
    let before = links(&layout);

    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(
        failures, 0,
        "an unpublished pin is a hold, never a failure: {:?}",
        report.groups
    );
    assert_eq!(
        *outcome_of(&report, "tb"),
        crate::TxnOutcome::Unpublished {
            member: "tb".into(),
            build: 6,
            triple: current_triple().into(),
        },
        "the group is held, not aborted"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        (active.get("ta").copied(), active.get("tb").copied()),
        (Some(3), Some(5)),
        "both members stay on their current builds"
    );
    assert_eq!(links(&layout), before, "the live links never moved");
    for (p, new) in [("ta", 4), ("tb", 6)] {
        assert!(!layout.build_dir(p, new).exists(), "{p}@{new} was staged");
        assert!(!layout.staging_dir(p).exists(), "{p} touched staging/");
    }
    assert!(
        report.resolved_assets.is_empty(),
        "nothing was resolved for download: {:?}",
        report.resolved_assets
    );
    let triple = current_triple();
    let status = crate::status::read(&layout).expect("recorded");
    assert_eq!(
        status.programs["tb"].state,
        format!("held: pinned build 6 is not published for {triple}; staying on build 5")
    );
    assert_eq!(
        status.programs["ta"].state,
        format!("held: tb's pinned build 6 is not published for {triple}; staying on build 3")
    );
    for (p, b) in [("ta", 3u64), ("tb", 5u64)] {
        assert_eq!(
            status.programs[p].installed_build,
            Some(b),
            "{p}'s build stays on the row"
        );
        assert_eq!(
            status.programs[p].tree_root,
            format!("root{b}"),
            "{p}'s attestation stays on the row"
        );
    }
    assert!(
        crate::doctor::recorded_problems(Some(&status)).is_empty(),
        "a held row is never a doctor fault: {:?}",
        crate::doctor::recorded_problems(Some(&status))
    );

    // The next tick says the same thing and still counts nothing.
    let (_, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0, "the hold is steady across passes");

    // The channel steps back to the builds this host runs: the UpToDate pass lifts the
    // held rows to the ordinary managed state, build and attestation kept.
    write_group_index(&dir, "ta = 3, tb = 5", &[]);
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    assert_eq!(*outcome_of(&report, "tb"), crate::TxnOutcome::UpToDate);
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ta"].state, crate::state::managed(3, 51));
    assert_eq!(status.programs["tb"].state, crate::state::managed(5, 51));
    assert_eq!(status.programs["tb"].tree_root, "root5");

    // The build publishes for this triple and the channel moves on again: the next
    // pass moves the whole tuple.
    write_group_index(&dir, "ta = 4, tb = 6", &[]);
    write_pkg(&dir, "tb", 6);
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    assert_eq!(
        *outcome_of(&report, "tb"),
        crate::TxnOutcome::Applied(vec!["ta".into(), "tb".into()])
    );
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ta"].state, crate::state::managed(4, 51));
    assert_eq!(status.programs["tb"].state, crate::state::managed(6, 51));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// …but a hold never keeps a revoked build running: quietly holding the tuple would leave
/// a yanked build live on exactly the host that has no replacement. The pass disables that
/// build's commands and aborts loudly instead, and stays loud — later passes hold the tuple
/// quietly for the member still installed, while the disabled row is still a doctor fault.
#[test]
fn an_unpublished_pin_never_quietly_holds_a_revoked_current_build() {
    let dir = scratch("update-held-revoked");
    write_installed_group_registry(&dir, "riscv64gc-unknown-linux-gnu", &["ta@3"]);
    let layout = temp_layout("update-held-revoked");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_installed_group_live(&layout);

    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(
        failures, 1,
        "a revoked build with no replacement here is a fault: {:?}",
        report.groups
    );
    assert!(
        matches!(
            outcome_of(&report, "ta"),
            crate::TxnOutcome::Aborted { failed, during_flip: false, why }
                if failed == "ta" && why.contains("is not published for")
        ),
        "never a quiet hold over a revoked build — and the abort names what it disabled, \
         and why: {:?}",
        outcome_of(&report, "ta")
    );
    // The line this pass printed for ta names the build this triple lacks: tb's new
    // pin. ta's own replacement, ta@4, is published here.
    let triple = current_triple();
    assert_eq!(
        crate::flow::recalled_unpublished_notice("ta", "tb", 6, triple),
        format!(
            "atpkg: ta was recalled and its coherence group's new pin (tb build 6) is not \
             published for {triple} — its commands are disabled until it is."
        )
    );
    // A live shim is an `exec` stub too — a regular file — so what marks the tombstone
    // is that it forwards NOWHERE and says why, and that the store stops counting the
    // revoked build as live.
    let shim = layout.shim(&crate::store::ToolName::new("ta").unwrap());
    assert_eq!(
        crate::platform::resolve_shim(&shim),
        None,
        "the revoked ta@3 must be disabled — its shim replaced by a tombstone"
    );
    let out = std::process::Command::new(&shim).output().unwrap();
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("yanked/revoked"),
        "…that refuses to run and says why: {out:?}"
    );
    assert!(
        !crate::active_builds(&layout).contains_key("ta"),
        "the revoked build no longer reads as live"
    );
    assert_eq!(
        crate::active_builds(&layout).get("tb").copied(),
        Some(5),
        "the valid sibling keeps running"
    );
    for (p, new) in [("ta", 4), ("tb", 6)] {
        assert!(!layout.build_dir(p, new).exists(), "{p}@{new} was staged");
        assert!(!layout.staging_dir(p).exists(), "{p} touched staging/");
    }
    let status = crate::status::read(&layout).unwrap();
    assert!(
        status.programs["ta"].state.starts_with("aborted: stage — "),
        "{}",
        status.programs["ta"].state
    );
    assert!(
        status
            .outcome
            .starts_with("group rustc: aborted at ta during stage — "),
        "{}",
        status.outcome
    );
    let problems = crate::doctor::recorded_problems(Some(&status));
    assert!(
        problems.len() == 1 && problems[0].starts_with("ta: aborted: stage"),
        "…and doctor counts it against the disabled program: {problems:?}"
    );

    // The next pass: the tombstone took ta out of `active_builds`, so only tb is seen
    // installed and tb's unpublished pin holds the tuple quietly. ta must not go quiet
    // with it — still disabled, and still a doctor fault on its own row.
    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0, "{:?}", report.groups);
    assert_eq!(
        *outcome_of(&report, "tb"),
        crate::TxnOutcome::Unpublished {
            member: "tb".into(),
            build: 6,
            triple: triple.into(),
        }
    );
    assert_eq!(
        crate::platform::resolve_shim(&shim),
        None,
        "ta stays disabled"
    );
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(
        status.programs["tb"].state,
        crate::state::held_unpublished(None, 6, triple, 5)
    );
    assert!(
        status.programs["ta"].state.starts_with("aborted: stage — "),
        "the disabled program's row never reads as healthy: {}",
        status.programs["ta"].state
    );
    let problems = crate::doctor::recorded_problems(Some(&status));
    assert!(
        problems.len() == 1 && problems[0].starts_with("ta: aborted: stage"),
        "doctor still counts the disabled program: {problems:?}"
    );
    // …AND SO DOES THE SET-COMPLETION PRESCAN THAT RUNS NEXT IN THE SAME PASS on an
    // adopted machine: ta is not installed any more and not published here, so the
    // prescan counts it unserved — but its row is a fault this machine recorded on
    // purpose, and repainting it `unavailable on <triple>: …` (a benign state the
    // doctor does not match) hid the fault before anyone read it. The prescan's
    // measurement is the one below: the recorded row, or a tombstone shim.
    assert!(
        program_disabled_here(&layout, Some(&status), "ta"),
        "ta reads as disabled: its row says so and its shim is a tombstone"
    );
    assert!(
        !program_disabled_here(&layout, Some(&status), "tb"),
        "tb is held, not disabled"
    );
    // Either measurement alone is enough: a row that says it with no shim on disk,
    // and a tombstone shim under a row that says nothing.
    let mut rows = status.clone();
    rows.programs.remove("ta");
    assert!(
        program_disabled_here(&layout, Some(&rows), "ta"),
        "the tombstone shim alone reads as disabled"
    );
    std::fs::remove_file(&shim).unwrap();
    assert!(
        program_disabled_here(&layout, Some(&status), "ta"),
        "the recorded `aborted: stage` row alone reads as disabled"
    );
    assert!(!program_disabled_here(&layout, Some(&rows), "ta"));
    assert!(!program_disabled_here(&layout, None, "ta"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The hold is only for a pin this host cannot run: the same installed tuple whose new pin
/// is published for this triple upgrades as it always did — staged whole, flipped whole,
/// managed rows naming the new index, the new signed roots recorded.
#[test]
fn an_installed_group_whose_new_pin_is_published_here_upgrades_as_before() {
    let dir = scratch("update-group-served");
    write_installed_group_registry(&dir, current_triple(), &[]);
    let layout = temp_layout("update-group-served");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_installed_group_live(&layout);

    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    assert_eq!(
        *outcome_of(&report, "ta"),
        crate::TxnOutcome::Applied(vec!["ta".into(), "tb".into()])
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        (active.get("ta").copied(), active.get("tb").copied()),
        (Some(4), Some(6))
    );
    assert_eq!(
        report.resolved_assets.get("tb").map(String::as_str),
        Some("tb-6.tar.zst")
    );
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ta"].state, crate::state::managed(4, 51));
    assert_eq!(status.programs["tb"].state, crate::state::managed(6, 51));
    for p in ["ta", "tb"] {
        let root = &status.programs[p].tree_root;
        assert!(
            !root.is_empty() && !root.starts_with("root"),
            "{p}'s new signed root is recorded: {root:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The hold reads only the members the pass would move. `ta` is up to date on build 3,
/// whose signed manifest has no row for this triple; `tb`'s current build 5 is yanked, so
/// `decide` force-upgrades it to 6, which is published here. Nothing of ta's is staged, so
/// tb moves to 6 as before: no hold, no abort, no failure, and ta's commands keep working.
#[test]
fn an_up_to_date_member_never_holds_or_aborts_a_siblings_upgrade() {
    let dir = scratch("update-uptodate-unserved");
    write_group_index(&dir, "ta = 3, tb = 6", &["tb@5"]);
    write_pkg_for_triple(&dir, "ta", 3, "riscv64gc-unknown-linux-gnu");
    write_pkg(&dir, "tb", 6);
    let layout = temp_layout("update-uptodate-unserved");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_installed_group_live(&layout);

    let (report, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0, "{:?}", report.groups);
    assert_eq!(
        *outcome_of(&report, "tb"),
        crate::TxnOutcome::Applied(vec!["tb".into()]),
        "the sibling's force-upgrade applies as before"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        (active.get("ta").copied(), active.get("tb").copied()),
        (Some(3), Some(6))
    );
    let shim = layout.shim(&crate::store::ToolName::new("ta").unwrap());
    assert!(
        crate::platform::resolve_shim(&shim).is_some(),
        "ta's commands were never disabled"
    );
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ta"].state, crate::state::managed(3, 51));
    assert_eq!(status.programs["tb"].state, crate::state::managed(6, 51));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The hold needs proof bound to the pin. A genuinely signed manifest served where tb@6's
/// belongs, but naming another build or another program, says nothing about tb@6: the pass
/// takes the loud path — the stage refuses the mismatch, one failure, the ordinary
/// `aborted: stage` row — never the quiet hold.
#[test]
fn a_manifest_that_does_not_bind_to_the_pin_never_proves_it_unpublished() {
    for (label, prog, build) in [("build", "tb", 7u64), ("program", "ta", 6u64)] {
        let dir = scratch(&format!("update-unbound-{label}"));
        write_group_index(&dir, "ta = 4, tb = 6", &[]);
        write_pkg(&dir, "ta", 4);
        write_pkg_for_triple(&dir, prog, build, "riscv64gc-unknown-linux-gnu");
        for ext in ["toml", "toml.sig"] {
            std::fs::rename(
                dir.join(format!("pkg-{prog}-{build}.{ext}")),
                dir.join(format!("pkg-tb-6.{ext}")),
            )
            .unwrap();
        }
        let layout = temp_layout(&format!("update-unbound-{label}"));
        let fetcher = crate::DirFetcher::new(dir.clone());
        make_installed_group_live(&layout);

        let (report, failures) = update_pass(&layout, &fetcher);
        assert_eq!(
            failures, 1,
            "{label}: an unbound manifest proves nothing, so the stage fails loudly: {:?}",
            report.groups
        );
        assert!(
            matches!(
                outcome_of(&report, "tb"),
                crate::TxnOutcome::Aborted { failed, during_flip: false, .. } if failed == "tb"
            ),
            "{label}: the old loud path, never a quiet hold: {:?}",
            outcome_of(&report, "tb")
        );
        let active = crate::active_builds(&layout);
        assert_eq!(
            (active.get("ta").copied(), active.get("tb").copied()),
            (Some(3), Some(5)),
            "{label}: nothing moved"
        );
        let status = crate::status::read(&layout).unwrap();
        assert!(
            status.programs["tb"].state.starts_with("aborted: stage"),
            "{label}: {}",
            status.programs["tb"].state
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// AN OLDER INDEX'S EXTRA IS A DEFAULT-SET MEMBER (design §5.3(c), 2026-09-24): a
/// program the signed index still lists with `extra = true` is planned, installed and
/// recorded by the default-set pass like any other — no consent stub, no opt-in.
#[cfg(unix)]
#[test]
fn an_older_index_extra_installs_as_a_default_set_member() {
    let dir = scratch("extra-retired");
    write_registry_with_extra(&dir, "stable", current_triple());
    let layout = temp_layout("extra-retired");
    let cfg = crate::config::PackagesConfig::default();
    let fetcher = crate::DirFetcher::new(dir.clone());
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.failures, 0);
    let active = crate::active_builds(&layout);
    assert_eq!(active.get("ay").copied(), Some(18));
    assert_eq!(active.get("xc").copied(), Some(9), "installed unasked");
    assert!(!crate::stub::pending_stub_exists(&layout, "xc"));
    let row = crate::status::read(&layout).unwrap().programs["xc"].clone();
    assert!(
        crate::state::managed_pin(&row.state).is_some_and(|(b, _)| b == 9),
        "{}",
        row.state
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// THE ONE CHANNEL: the bootstrap resolves `stable` ([`crate::config::CHANNEL`]) —
// a registry publishing only `nightly` installs nothing, loudly, and a retired
// `[packages].channel = "nightly"` in the file changes nothing (2026-09-23).
#[test]
fn default_set_bootstrap_uses_the_one_channel() {
    let dir = scratch("channel");
    write_registry(&dir, "nightly");
    let fetcher = crate::DirFetcher::new(dir.clone());
    for (label, cfg) in [
        ("channel-default", crate::config::PackagesConfig::default()),
        (
            "channel-retired-key",
            crate::config::parse_packages("[packages]\nchannel = \"nightly\"\n"),
        ),
    ] {
        let layout = temp_layout(label);
        let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
        assert!(
            failures > 0,
            "{label}: a missing channel is loud, not silent"
        );
        assert!(crate::active_builds(&layout).is_empty(), "{label}");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
    // The stable channel threads through and installs.
    let stable = scratch("channel-stable");
    write_registry(&stable, "stable");
    let layout = temp_layout("channel-stable");
    let failures = install_default_set(
        &layout,
        &crate::DirFetcher::new(stable.clone()),
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    )
    .failures;
    assert_eq!(failures, 0);
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(18),
        "the one channel reached the install request"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&stable);
    let _ = std::fs::remove_dir_all(&dir);
}

// AN UNRESOLVED INDEX IS NOT A CHECK. An adopted machine with an EMPTY store runs
// `update` through the set-completion lane alone; offline (or on a channel the index
// does not publish) that lane fails before it checks anything, and the failed-pass
// exit used to stamp `last_success_at` regardless — silencing "never checked" and
// giving `doctor` a 0-day age on a machine that had never reached the index. The
// outcome now says whether the index resolved, the lane records the apply lane's
// `*index*` row, and a resolved pass retires it.
#[test]
fn an_empty_store_pass_that_never_resolved_the_index_is_not_stamped() {
    let cfg = crate::config::PackagesConfig::default();
    // Offline: nothing at the index URL.
    let empty = scratch("unresolved-offline");
    let layout = temp_layout("unresolved-offline");
    let out = install_default_set(
        &layout,
        &crate::DirFetcher::new(empty.clone()),
        &test_anchor(),
        &cfg,
        0,
    );
    assert_eq!(out.failures, 1);
    assert!(
        out.index_error.is_some(),
        "an unreachable index is reported"
    );
    assert!(
        !settle_net_lane_index(&layout, false, out.index_error.as_deref()),
        "an empty store's pass resolved nothing"
    );
    let row = crate::status::read(&layout).expect("the *index* row is written");
    assert!(
        row.programs
            .get("*index*")
            .is_some_and(|p| p.state.starts_with("error: ")),
        "the lane records the apply lane's own *index* failure row"
    );
    assert!(
        crate::status::never_checked(&layout),
        "an unresolved pass is not a completed check"
    );
    // …but a miss after the apply lane resolved this pass changes nothing.
    let quiet = temp_layout("unresolved-after-apply");
    assert!(settle_net_lane_index(&quiet, true, Some("offline")));
    assert!(crate::status::read(&quiet).is_none(), "no row re-opened");
    let _ = std::fs::remove_dir_all(&quiet.prefix);
    // A channel the index does not publish: resolved, but nothing to act on — the
    // apply lane's `Err(NoChannel)`, so the same verdict.
    let dir = scratch("unresolved-channel");
    write_registry(&dir, "nightly");
    let fetcher = crate::DirFetcher::new(dir.clone());
    let nightly_only = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert!(
        nightly_only.index_error.is_some(),
        "a missing channel is no check"
    );
    // The index resolves on the one channel: the pass checked, and the failure row it
    // disproved is retired.
    let stable = scratch("unresolved-channel-stable");
    write_registry(&stable, "stable");
    let ok = install_default_set(
        &layout,
        &crate::DirFetcher::new(stable.clone()),
        &test_anchor(),
        &cfg,
        0,
    );
    assert_eq!(ok.index_error, None);
    assert!(settle_net_lane_index(
        &layout,
        false,
        ok.index_error.as_deref()
    ));
    assert!(
        !crate::status::read(&layout)
            .unwrap()
            .programs
            .contains_key("*index*"),
        "a resolved pass retires the *index* row"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_dir_all(&stable);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Write a root-signed index whose default set is the `rustc`-locked coherence
/// tuple `ta`+`tb` plus the ungrouped singleton `ay` — the §7 bootstrap fixture.
/// `write_tb_pkg = false` leaves tb's pkg manifest unpublished, so its stage
/// fails and the group transaction must abort WHOLE (never a split tuple).
fn write_group_registry(dir: &Path, channel: &str, write_tb_pkg: bool) {
    let index_body = format!(
        "schema = 2\nindex_build = 51\nvalid_until = \"2026-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n\
         [programs.ta]\nrepo = \"ta\"\ncoherence_group = \"rustc\"\n\
         [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"{channel}\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ ay = 18, ta = 4, tb = 6 }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    std::fs::write(dir.join("index.toml"), index_body.as_bytes()).unwrap();
    // The index is MACHINE-signed now; only the roster is signed by the master.
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, index_body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
    write_pkg(dir, "ay", 18);
    write_pkg(dir, "ta", 4);
    if write_tb_pkg {
        write_pkg(dir, "tb", 6);
    }
}

// §7 bootstrap happy path: a missing coherence tuple fresh-installs WHOLE through
// the group transaction (one resolved index for the whole pass), alongside the
// ungrouped singleton; the durable floor advances to the one trusted index.
#[test]
fn default_set_bootstrap_installs_a_coherence_group_atomically() {
    let dir = scratch("group-ok");
    write_group_registry(&dir, "stable", true);
    let layout = temp_layout("group-ok");
    let cfg = crate::config::PackagesConfig::default();
    let fetcher = crate::DirFetcher::new(dir.clone());
    let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
    assert_eq!(failures, 0);
    let active = crate::active_builds(&layout);
    assert_eq!(active.get("ta").copied(), Some(4), "tuple member installed");
    assert_eq!(active.get("tb").copied(), Some(6), "tuple member installed");
    assert_eq!(active.get("ay").copied(), Some(18), "singleton installed");
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 51);
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// §7 all-or-nothing: a tuple member whose pkg manifest is unpublished aborts the
// WHOLE group — the sibling that staged fine is neither activated nor left on
// disk (a complete-but-inactive build would mis-read as active next run), while
// the ungrouped singleton still installs. Exactly ONE loud failure for the group.
#[test]
fn default_set_bootstrap_never_splits_a_coherence_group() {
    let dir = scratch("group-abort");
    write_group_registry(&dir, "stable", false);
    let layout = temp_layout("group-abort");
    let cfg = crate::config::PackagesConfig::default();
    let fetcher = crate::DirFetcher::new(dir.clone());
    let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
    assert_eq!(failures, 1, "one failure for the aborted group");
    let active = crate::active_builds(&layout);
    assert!(
        !active.contains_key("ta"),
        "no partial tuple: ta not active"
    );
    assert!(
        !active.contains_key("tb"),
        "no partial tuple: tb not active"
    );
    assert!(
        !layout.build_dir("ta", 4).exists(),
        "the staged sibling was discarded on abort"
    );
    assert_eq!(
        active.get("ay").copied(),
        Some(18),
        "the singleton still installs"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A [`crate::DirFetcher`] whose first request for `program`'s manifest fails, as a dropped
/// connection does; every later request is served. Nothing remembers a failed fetch, so a
/// prescan that met the failure proved nothing and a later walk, asking again, succeeds.
struct FirstManifestFetchFails {
    inner: crate::DirFetcher,
    program: &'static str,
    tripped: std::cell::Cell<bool>,
}
impl crate::flow::Fetcher for FirstManifestFetchFails {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        crate::flow::Fetcher::index_candidates(&self.inner)
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        if program == self.program && !self.tripped.replace(true) {
            return Err(format!("{program}@{build}: connection reset"));
        }
        crate::flow::Fetcher::pkg_manifest(&self.inner, repo, program, build)
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        crate::flow::Fetcher::download(&self.inner, repo, asset, dest)
    }
    fn source_id(&self) -> String {
        crate::flow::Fetcher::source_id(&self.inner)
    }
}

// The bootstrap arm of the missing-triple hold. A partly installed tuple: ta@3 is live,
// tb is missing, the channel pins ta = 4 (published only for a foreign triple) and tb = 6.
// The prescan's request for ta@4's manifest fails and proves nothing, so the walk fetches
// it and finds no artifact for this triple. A tuple that cannot fully exist here is a clean
// skip, never a failure: nothing staged, nothing resolved.
#[test]
fn default_set_bootstrap_skips_a_partial_tuple_whose_installed_pin_is_unpublished() {
    let dir = scratch("group-partial-unpublished");
    write_group_index(&dir, "ta = 4, tb = 6", &[]);
    write_pkg_for_triple(&dir, "ta", 4, "riscv64gc-unknown-linux-gnu");
    write_pkg(&dir, "tb", 6);
    let layout = temp_layout("group-partial-unpublished");
    make_live(&layout, "ta", 3);
    let fetcher = FirstManifestFetchFails {
        inner: crate::DirFetcher::new(dir.clone()),
        program: "ta",
        tripped: std::cell::Cell::new(false),
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert!(
        fetcher.tripped.get(),
        "a prescan's request met the dropped fetch"
    );
    assert_eq!(
        out.failures, 0,
        "an unpublished pin is a clean skip, never a failure"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(active.get("ta").copied(), Some(3), "ta keeps its build");
    assert!(!active.contains_key("tb"), "tb is not installed");
    for (p, b) in [("ta", 4u64), ("tb", 6u64)] {
        assert!(!layout.build_dir(p, b).exists(), "{p}@{b} was staged");
        assert!(!layout.staging_dir(p).exists(), "{p} touched staging/");
    }
    assert!(
        out.resolved_assets.is_empty(),
        "nothing was resolved for download: {:?}",
        out.resolved_assets
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// §7 narrowing: an `exclude` that leaves part of a coherence tuple out refuses
// the WHOLE group (loud diagnostic, not a failure) — a deliberately partial
// tuple is exactly the split the transaction exists to prevent — while the
// ungrouped singleton still installs.
// The refusal is decided at PLANNING, not at the group arm:
// the refused members ride out on the outcome (`skipped_narrowed`), are NOT
// counted as still-pending (`remaining_missing`), and no pending stub
// survives for them — a stub is a name on PATH promising exactly the
// install the pass just refused. Observed live 2026-08-27: the arm-time
// decision announced trust-cg/ir/vc, refused them one line later, left
// their stubs behind, and the summary then claimed the set complete.
#[test]
fn a_narrowed_tuple_is_refused_at_planning_and_reported_on_the_outcome() {
    let dir = scratch("group-narrowed-plan");
    write_group_registry(&dir, "stable", true);
    let layout = temp_layout("group-narrowed-plan");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["tb".into()]),
        ..Default::default()
    };
    let fetcher = crate::DirFetcher::new(dir.clone());
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.failures, 0, "a narrowed tuple is never a failure");
    let active = crate::active_builds(&layout);
    assert!(
        !active.contains_key("ta") && !active.contains_key("tb"),
        "no partial tuple over a narrowing"
    );
    assert_eq!(
        active.get("ay").copied(),
        Some(18),
        "the singleton still installs"
    );
    assert_eq!(
        out.skipped_narrowed,
        vec!["ta".to_string()],
        "the refused member is reported, not silently dropped"
    );
    // The vendor programs are wanted from the compiled table and still coming through
    // their own lane (design §1.5); no index member is.
    assert!(
        out.remaining_missing
            .iter()
            .all(|p| crate::vendor_direct::is_vendor(p)),
        "a refused member is not 'still coming': {:?}",
        out.remaining_missing
    );
    assert!(
        !crate::stub::pending_stub_exists(&layout, "ta"),
        "no stub may promise the install this pass refused"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// The explicit toolset verb clears removal/decline markers BEFORE its pass:
// `install_default_set` narrows `wanted` by those very markers, so the old
// after-ordering honoured the old mind for exactly the pass that was asked
// to change it — a legacy `trust` removal kept trust-cg/ir/vc out of the
// explicit toolset install, which then reported the set complete
// (2026-08-27, live). A source-shape test in the house style, because the
// verb reads the GLOBAL layout and so cannot be driven by a scratch one.
#[test]
fn the_toolset_verb_changes_its_mind_before_its_pass() {
    let src = include_str!("cli.rs");
    let start = src
        // The pass, not the wrappers that answer the announcement and hand it the
        // process's store: they have neither the clears nor the pass call in them.
        .find("fn install_toolset(")
        .expect("the toolset verb");
    let end = src[start..]
        .find("\nfn ")
        .map(|i| start + i)
        .expect("a function after the verb");
    let body = &src[start..end];
    let clear = body
        .find("clear_removed")
        .expect("the change-of-mind clear");
    let pass = body
        .find("= install_default_set_with_path(")
        .expect("the pass call");
    assert!(
        clear < pass,
        "removal markers must be cleared BEFORE the pass that narrows by them"
    );
    assert!(
        body.find("clear_decline").expect("the decline clear") < pass,
        "the decline must be cleared BEFORE the pass that honours it"
    );
}

// A LEGACY `*seed*` offer row — written only by the deleted sealed-payload lane —
// is swept ROW-ONLY (the pass's own outcome sentence survives), and by the seed verb
// on the machines no set-completing pass reaches.
#[test]
fn a_legacy_seed_offer_row_is_retired_row_only() {
    let layout = temp_layout("seed-row-retire");
    // No row: retiring writes nothing.
    retire_seed_row(&layout);
    assert!(
        crate::status::read(&layout).is_none(),
        "retiring an absent row must not create a status record"
    );
    record_status(
        &layout,
        "*seed*",
        crate::ProgramStatus {
            installed_build: None,
            state: "pending-consent: old-a, old-b".into(),
            tree_root: String::new(),
        },
        "ALab toolchain ready: old-a, old-b — Install ALab toolset below".into(),
    );
    record_outcome(&layout, "up to date (index build 21)".into());
    retire_seed_row(&layout);
    let status = crate::status::read(&layout).expect("a status record");
    assert!(
        !status.programs.contains_key("*seed*"),
        "a clean pass retires the legacy offer"
    );
    assert_eq!(
        status.outcome, "up to date (index build 21)",
        "retiring the row is ROW-ONLY: the pass's own outcome survives"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

// A `[packages.links]` owner/repo override for a program the signed index does NOT
// name can never install anything (reachability §5 is structural); the bootstrap
// only says so loudly. Fail-closed: no unsigned installs from slugs.
#[test]
fn unindexed_repo_override_never_installs() {
    let dir = scratch("unindexed");
    write_registry(&dir, "stable");
    let layout = temp_layout("unindexed");
    let cfg = crate::config::parse_packages("[packages.links]\nghost = \"alabsystems/ghost\"\n");
    let fetcher = crate::DirFetcher::new(dir.clone());
    let failures = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0).failures;
    assert_eq!(
        failures, 0,
        "the refusal is a loud diagnostic, not a loop failure"
    );
    assert!(
        !crate::active_builds(&layout).contains_key("ghost"),
        "an unindexed program is unreachable by construction"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// [packages.links] reconciliation: a config checkout path becomes a managed
// dev-link (idempotent across passes); a missing checkout links nothing; an
// invalid value acts on nothing.
#[test]
fn reconcile_links_creates_and_reasserts_config_links() {
    let layout = temp_layout("reconcile");
    let co = checkout("reconcile", "ay");
    let cfg = crate::config::parse_packages(&format!(
        "[packages.links]\nay = {:?}\nmissing = \"/nonexistent/checkout\"\nbad = \"a b\"\n",
        co.display().to_string()
    ));
    reconcile_links(&layout, &cfg);
    assert!(
        crate::is_linked(&layout, "ay"),
        "config path became a dev-link"
    );
    assert_eq!(
        crate::linked_checkout(&layout, "ay").unwrap(),
        std::path::absolute(&co).unwrap()
    );
    assert!(
        !crate::is_linked(&layout, "missing"),
        "missing checkout links nothing"
    );
    assert!(
        !crate::is_linked(&layout, "bad"),
        "invalid value acts on nothing"
    );
    // Idempotent second pass: still linked, same checkout, and a newly-built bin
    // is picked up by the quiet refresh.
    std::fs::write(co.join("target/release").join("ay2"), b"#!/bin/true\n").unwrap();
    let cfg2 = crate::config::parse_packages(&format!(
        "[packages.links]\nay = {:?}\n",
        co.display().to_string()
    ));
    reconcile_links(&layout, &cfg2);
    assert!(crate::is_linked(&layout, "ay"));
    assert_eq!(
        crate::linked_checkout(&layout, "ay").unwrap(),
        std::path::absolute(&co).unwrap()
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&co);
}

// The FOREIGN-link refusal: a hand-made dev-link pointing at a DIFFERENT checkout
// is never re-pointed by config — the link (and its shim target) survive untouched.
#[test]
fn reconcile_links_refuses_to_touch_a_foreign_link() {
    let layout = temp_layout("foreign");
    let hand = checkout("foreign-hand", "ay");
    let config_wants = checkout("foreign-cfg", "ay");
    crate::link(&layout, "ay", &hand, &[PathBuf::from("target/release/ay")]).unwrap();
    let cfg = crate::config::parse_packages(&format!(
        "[packages.links]\nay = {:?}\n",
        config_wants.display().to_string()
    ));
    reconcile_links(&layout, &cfg);
    assert_eq!(
        crate::linked_checkout(&layout, "ay").unwrap(),
        std::path::absolute(&hand).unwrap(),
        "the hand-made link's checkout is untouched"
    );
    assert_eq!(
        crate::platform::resolve_shim(&layout.shim(&crate::store::ToolName::new("ay").unwrap()))
            .unwrap(),
        std::path::absolute(&hand)
            .unwrap()
            .join("target/release/ay"),
        "the shim still points into the hand-made checkout"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&hand);
    let _ = std::fs::remove_dir_all(&config_wants);
}

// -----------------------------------------------------------------------
// `__pending` — the five honest states, the honesty edge, escape stripping
// (R6; design §5).
// -----------------------------------------------------------------------

fn now_secs() -> u64 {
    aterm_time::unix_secs().unwrap()
}

/// A live-looking progress snapshot: this test process's pid, a fresh heartbeat.
fn live_snapshot(programs: &str, queue: &str) -> String {
    format!(
        "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"heartbeat_unix\":{},\
         \"overall\":{{\"programs_done\":2,\"programs_total\":9}},\
         \"queue\":{queue},\"programs\":{programs}}}",
        std::process::id(),
        now_secs()
    )
}

fn bump_contents(l: &crate::store::Layout) -> String {
    std::fs::read_to_string(l.bump_file()).unwrap_or_default()
}

/// [`pending_state`] collected NON-interactively (no TTY, no aterm): the lines
/// only — the spelling of the four default-set states and the honesty edge.
fn pending_state_lines(layout: &crate::store::Layout, tool: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut say = |line: String| out.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: false,
    };
    pending_state(layout, tool, &mut io);
    out
}

/// [`pending_state`] for a person at a terminal who can WAIT: the lines and what the
/// stub does next.
fn pending_state_waiting(layout: &crate::store::Layout, tool: &str) -> (Vec<String>, PendingNext) {
    let mut out: Vec<String> = Vec::new();
    let mut say = |line: String| out.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: true,
    };
    let next = pending_state(layout, tool, &mut io);
    (out, next)
}

/// State: NOT STARTED — no progress file, no status row. The honest line names
/// both next acts, and the bump is still written so the next pass front-loads
/// the program the user actually wanted.
#[test]
fn pending_not_started_names_the_next_act() {
    let l = temp_layout("pend-notstarted");
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines[0].contains("trust:"),
        "leads with what the tool IS: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|x| x.contains("nothing is installing right now")
                && x.contains("open aterm")
                && x.contains("aterm pkg update")),
        "the not-started state names its fixes: {lines:?}"
    );
    assert_eq!(
        bump_contents(&l),
        "trust\n",
        "the wish is recorded for the next pass"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// State: INSTALLING NOW — a live snapshot's download row renders live MB and
/// the overall count.
#[test]
fn pending_installing_now_shows_live_bytes() {
    let l = temp_layout("pend-installing");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"trust\":{\"phase\":\"download\",\"bytes_done\":4100000,\"bytes_total\":9800000}}",
            "[\"trust\"]",
        ),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines.iter().any(|x| x.contains("2 of 9 done")),
        "overall progress renders: {lines:?}"
    );
    assert!(
        lines.iter().any(
            |x| x.contains("trust is being installed (41%, 4.1 of 9.8 MB)")
                && x.contains("try again in a minute")
        ),
        "live MB from the snapshot, in plain words: {lines:?}"
    );
    assert!(
        !lines.iter().any(|x| x.contains("NOW")),
        "nothing shouted: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A PERSON AT A TERMINAL WAITS: the same live download is not "try again later" but
/// a wait that ends in the program running ([`PendingNext::WaitThenRun`]); a queued
/// program is bumped first so the wait is short. A failed row is still said, never
/// waited on.
#[test]
fn pending_on_a_terminal_waits_for_a_program_on_its_way() {
    let l = temp_layout("pend-wait");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"robi\":{\"phase\":\"download\"},\"trust\":{\"phase\":\"queued\"},\
             \"ty\":{\"phase\":\"failed\",\"error\":\"disk full\"}}",
            "[\"robi\",\"trust\"]",
        ),
    )
    .unwrap();
    let (lines, next) = pending_state_waiting(&l, "trust");
    assert_eq!(next, PendingNext::WaitThenRun, "{lines:?}");
    assert!(
        lines.iter().any(|x| x.contains("trust is on its way")
            && x.contains("starts as soon as it is installed")
            && x.contains("Ctrl-C")),
        "{lines:?}"
    );
    assert_eq!(
        bump_contents(&l),
        "trust\n",
        "queued: bumped before the wait"
    );
    let (lines, next) = pending_state_waiting(&l, "robi");
    assert_eq!(next, PendingNext::WaitThenRun, "downloading: {lines:?}");
    let (lines, next) = pending_state_waiting(&l, "ty");
    assert_eq!(next, PendingNext::Done, "a failure is said, not waited on");
    assert!(lines.iter().any(|x| x.contains("disk full")), "{lines:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// TIER-1: the stub's REAL decision ([`pending_wait_step`]) against the derived model
/// `AtpkgPendingWait`, at every reachable deciding state — the model's reads mapped
/// onto the function's inputs in the order `wait_then_run` makes them (the pass, then
/// the shim), which the scan at the end pins. Negative control: the decision the stub
/// could have made instead — "the pass ended, so it failed", asked before the shim —
/// disagrees with the model at a reachable state, so agreement is never vacuous.
#[test]
fn the_pending_wait_decision_refines_the_derived_model() {
    use aterm_spec::derive::atpkg_pending_wait_model;
    let model = atpkg_pending_wait_model();
    let mut seen = std::collections::BTreeSet::new();
    let mut frontier = vec![model.init_state()];
    while let Some(state) = frontier.pop() {
        if !seen.insert(state.clone()) {
            continue;
        }
        for action in &model.actions {
            let mut next = state.clone();
            if model.fire(action.name, &mut next) {
                frontier.push(next);
            }
        }
    }
    let deciding: Vec<_> = seen.iter().filter(|s| s["step"] == 1).collect();
    assert!(
        deciding.len() >= 6,
        "the explorer reached the decisions: {deciding:?}"
    );
    let modeled = |s: &std::collections::BTreeMap<&'static str, i64>| match [
        "DecideRun",
        "DecideFail",
        "DecideWait",
    ]
    .into_iter()
    .filter(|a| model.action_enabled(a, s))
    .collect::<Vec<_>>()[..]
    {
        ["DecideRun"] => WaitStep::Run,
        ["DecideFail"] => WaitStep::Fail,
        ["DecideWait"] => WaitStep::Wait,
        ref other => panic!("exactly one decision is enabled at {s:?}: {other:?}"),
    };
    let inputs = |s: &std::collections::BTreeMap<&'static str, i64>| {
        (s["seen"] == 1, s["shim"] == 1, s["row"] == 4)
    };
    for s in &deciding {
        let (running, installed, failed) = inputs(s);
        assert_eq!(
            pending_wait_step(running, installed, failed),
            modeled(s),
            "the real decision at {s:?}"
        );
    }
    // NEGATIVE CONTROL: ended-means-failed, asked before the shim.
    let mutant = |running: bool, installed: bool, failed: bool| {
        if failed || !running {
            WaitStep::Fail
        } else if installed {
            WaitStep::Run
        } else {
            WaitStep::Wait
        }
    };
    assert!(
        deciding.iter().any(|s| {
            let (running, installed, failed) = inputs(s);
            mutant(running, installed, failed) != modeled(s)
        }),
        "the comparison must be able to fail"
    );
    // The ORDER the mapping assumes: `wait_then_run` reads the pass, then the shim.
    let production = production_half(include_str!("cli.rs"));
    let start = production
        .find("\nfn wait_then_run(")
        .expect("wait_then_run");
    let body = &production[start..];
    let body = &body[..body[1..].find("\nfn ").map_or(body.len(), |i| i + 1)];
    let in_loop = &body[body.find("let outcome = loop").expect("the poll loop")..];
    let pass = in_loop
        .find("read_progress(layout)")
        .expect("the pass read");
    let shim = in_loop
        .find("crate::which(layout, tool)")
        .expect("the shim read");
    assert!(pass < shim, "the pass is read before the shim: {in_loop}");
}

/// A pass that installs something ELSE (a person's `update`, another program's
/// `install`) is no delisting: its snapshot never planned the default set, so the stub
/// answers as if nothing were installing this program — and still records the wish.
#[test]
fn pending_is_not_delisted_by_a_pass_that_plans_other_work() {
    let l = temp_layout("pend-other-pass");
    std::fs::write(
        l.progress_file(),
        live_snapshot("{\"ty\":{\"phase\":\"download\"}}", "[]")
            .replace("\"pass\":\"net\"", "\"pass\":\"update\""),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        !lines
            .iter()
            .any(|x| x.contains("no longer part of the default set")),
        "{lines:?}"
    );
    assert_eq!(bump_contents(&l), "trust\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// State: QUEUED — the row is queued behind a current program: position + the
/// BUMP claim, and the bump file really carries the name (a claimed bump must
/// have been written).
#[test]
fn pending_queued_bumps_and_says_so() {
    let l = temp_layout("pend-queued");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"robi\":{\"phase\":\"download\"},\"trust\":{\"phase\":\"queued\"}}",
            "[\"robi\",\"ty\",\"trust\"]",
        ),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines.iter().any(
            |x| x.contains("trust is next in line (it was 3 of 3), after robi")
                && x.contains("try again in a minute")
        ),
        "position + bump + current program: {lines:?}"
    );
    assert_eq!(bump_contents(&l), "trust\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// State: SKIPPED — a TERMINAL phase, never a wait. `Sink::finished` has already
/// retired the program from the queue and counted it done, so nothing later in this
/// pass installs it, and the reorder-only bump file cannot put it back. The stub says
/// the RECORDED reason (control-stripped, like every other untrusted echo) and claims
/// NO bump — it used to answer a member blocked by an unmet requirement with "queued
/// and now BUMPED to install next", a promise the running pass never kept.
#[test]
fn pending_skipped_says_the_recorded_reason_and_claims_no_bump() {
    let l = temp_layout("pend-skipped");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"robi\":{\"phase\":\"download\"},\
             \"trust\":{\"phase\":\"skipped\",\
             \"error\":\"blocked by xc: \\u001b[2Jextra — not installed\"}}",
            "[\"robi\"]",
        ),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    let said = lines
        .iter()
        .find(|x| x.contains("SKIPPED"))
        .expect("the skip renders");
    assert!(
        said.contains("blocked by xc") && said.contains("[2Jextra"),
        "the recorded reason surfaces, control-stripped: {said}"
    );
    assert!(!said.contains('\u{1b}'), "no ESC byte reaches the TTY");
    assert!(
        !lines.iter().any(|x| x.contains("next in line")),
        "a terminal skip is off the queue: no bump can restart it: {lines:?}"
    );
    assert_eq!(bump_contents(&l), "", "and no bump was written");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The same skip with NO recorded reason — a dev link, a narrowed tuple, an
/// already-current group: the honest generic line, its next act named, and still
/// neither a bump nor a claim of one.
#[test]
fn pending_skipped_without_a_reason_promises_no_bump_either() {
    let l = temp_layout("pend-skipped-bare");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"robi\":{\"phase\":\"download\"},\"trust\":{\"phase\":\"skipped\"}}",
            "[\"robi\"]",
        ),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("SKIPPED by this pass") && x.contains("aterm pkg install trust")),
        "the generic skip names its next act: {lines:?}"
    );
    assert!(
        !lines.iter().any(|x| x.contains("next in line")),
        "nothing in this pass installs it: {lines:?}"
    );
    assert_eq!(bump_contents(&l), "");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// State: FAILED — the recorded error surfaces WITH its next act, and control
/// characters in it are stripped before the TTY (escape-sequence injection).
#[test]
fn pending_failed_names_the_error_and_strips_escapes() {
    let l = temp_layout("pend-failed");
    record_status(
        &l,
        "trust",
        crate::ProgramStatus {
            installed_build: None,
            state: "error: mirror said \u{1b}[2Jno".into(),
            tree_root: String::new(),
        },
        "bootstrap install trust: failed".into(),
    );
    let lines = pending_state_lines(&l, "trust");
    let failed = lines
        .iter()
        .find(|x| x.contains("FAILED"))
        .expect("the failure renders");
    assert!(
        failed.contains("mirror said [2Jno"),
        "escapes stripped: {failed}"
    );
    assert!(!failed.contains('\u{1b}'), "no ESC byte reaches the TTY");
    assert!(
        failed.contains("fix: aterm pkg update"),
        "every failure names its next act"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A STALE snapshot renders only not-running states: yesterday's "downloading"
/// must never be today's claim, however live the file looks otherwise.
#[test]
fn pending_stale_heartbeat_renders_not_running() {
    let l = temp_layout("pend-stale");
    let stale = format!(
        "{{\"v\":1,\"pid\":{},\"heartbeat_unix\":{},\
         \"programs\":{{\"trust\":{{\"phase\":\"download\",\"bytes_done\":1}}}}}}",
        std::process::id(),
        now_secs() - crate::progress::HEARTBEAT_STALE_SECS - 5
    );
    std::fs::write(l.progress_file(), stale).unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("nothing is installing right now")),
        "stale = not running: {lines:?}"
    );
    assert!(
        !lines.iter().any(|x| x.contains("downloading")),
        "a dead installer's file never claims live progress: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// THE HONESTY EDGE: a live resolved-pass snapshot that does NOT plan the
/// program says "no longer part of the default set" — and claims NO bump,
/// because the reorder-only intersection would silently drop it.
#[test]
fn pending_delisted_name_is_told_the_truth_and_not_bumped() {
    let l = temp_layout("pend-delisted");
    std::fs::write(
        l.progress_file(),
        live_snapshot("{\"ty\":{\"phase\":\"download\"}}", "[\"ty\"]"),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "trust");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("no longer part of the default set")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|x| x.contains("next in line")),
        "no bump claim the installer would drop: {lines:?}"
    );
    assert_eq!(bump_contents(&l), "", "and no bump was written");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A name the shim gate refuses is never echoed back — the one universally safe
/// answer for it.
#[test]
fn pending_refuses_inadmissible_names() {
    let l = temp_layout("pend-refuse");
    let lines = pending_state_lines(&l, "../sudo");
    assert_eq!(
        lines,
        vec!["atpkg: that is not an installable program name".to_string()]
    );
    assert_eq!(bump_contents(&l), "");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A STUB AN OLDER CLIENT LAID FOR AN EXTRA (its retired marker line, design
/// §5.3(c)) tells the ordinary truths: every program the index names is a default-set
/// member now, so `__pending` says it is not installed yet and bumps it — no question,
/// no marker. An AGENT PROGRAM with no stub laid at all gets its vendor line the same way.
#[test]
fn pending_over_an_older_extra_stub_is_the_default_set_state() {
    let l = temp_layout("pend-extra-retired");
    let tool = crate::store::ToolName::new("vendorx").unwrap();
    crate::stub::write_pending_stub(&l, &tool).unwrap();
    let shim = l.shim(&tool);
    let body = std::fs::read_to_string(&shim).unwrap().replacen(
        "# atpkg pending-program stub v1\n",
        "# atpkg pending-program stub v1\n# atpkg extra: asks consent before installing\n",
        1,
    );
    std::fs::write(&shim, body).unwrap();
    let lines = pending_state_lines(&l, "vendorx");
    assert!(lines[0].contains("not installed yet"), "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|x| x.contains("EXTRA") || x.contains("[y/N]")),
        "{lines:?}"
    );
    assert_eq!(bump_contents(&l), "vendorx\n");
    let lines = pending_state_lines(&l, "claude");
    assert!(
        lines[0].contains("Anthropic Claude Code")
            && lines[0].contains("downloads.claude.ai")
            && lines[0].contains("not installed yet"),
        "{lines:?}"
    );
    assert_eq!(bump_contents(&l), "vendorx\nclaude\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

// -----------------------------------------------------------------------
// The priority queue — reorder-only by construction (design §4).
// -----------------------------------------------------------------------

fn plan_of(groups: &[&[&str]]) -> Vec<(usize, Vec<String>)> {
    groups
        .iter()
        .enumerate()
        .map(|(i, members)| (i, members.iter().map(|m| (*m).to_string()).collect()))
        .collect()
}

/// Bumped-first in bump order, plan order as the stable tiebreak; names outside
/// the plan have no effect at all.
#[test]
fn resort_bumped_orders_bumped_first_then_plan_order() {
    let mut plan = plan_of(&[&["ay"], &["clean"], &["ny"], &["ty"]]);
    resort_bumped(&mut plan, &["ty".to_string(), "clean".to_string()]);
    let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
    assert_eq!(order, vec![3, 1, 0, 2], "bump order first, then plan order");
    // Unknown names are inert — the reorder-only intersection already dropped
    // anything unplanned, and even raw they cannot move a thing.
    let mut plan = plan_of(&[&["ay"], &["clean"]]);
    resort_bumped(&mut plan, &["sudo".to_string(), "zzz".to_string()]);
    let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
    assert_eq!(order, vec![0, 1]);
}

/// Bumping ANY member bumps its WHOLE group — the tuple stays transactional,
/// the queue jump is group-granular.
#[test]
fn bumping_a_member_bumps_the_whole_group() {
    let mut plan = plan_of(&[&["ay"], &["trust", "trust-cg", "trust-ir"], &["ty"]]);
    resort_bumped(&mut plan, &["trust-ir".to_string()]);
    let order: Vec<usize> = plan.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        order,
        vec![1, 0, 2],
        "the group containing the bumped member moves whole"
    );
    assert_eq!(
        plan[0].1,
        vec![
            "trust".to_string(),
            "trust-cg".to_string(),
            "trust-ir".to_string()
        ],
        "membership untouched — activation stays all-or-nothing"
    );
}

/// `verify-pkg` IS THE CLIENT'S WHOLE CHAIN, NOT HALF OF IT.
///
/// `tools/atpkg-publish-lib.sh` and step 1 of `tools/atpkg-mirror-public.sh` call this
/// verb "the client's own rule" and refuse to ship what it refuses — but it checked the
/// SIGNATURE and printed OK, so a manifest that verifies and cannot be PARSED self-checked
/// OK, was mirrored, and was then refused by every client (`parse_pkg(&verified).ok()?` →
/// `None`) with no pipeline stage having said why. Every body below is signed by the
/// roster's own machine, so the only thing under test is what the verb does once the
/// signature holds; the refusal must also NAME the fix, which is what
/// `Reject::ShimEnv`/`RetiredKind` promise in their own docs.
#[test]
fn verify_pkg_refuses_a_signed_manifest_no_client_can_parse() {
    const HEAD: &str = "schema = 2\nprogram = \"claude\"\nversion = \"2.1.231\"\n\
                        build_number = 2026082601\nexposes = [\"claude\"]\n";
    const ROW: &str = "[[artifact]]\ntarget = \"aarch64-apple-darwin\"\nkind = \"binary\"\n\
                       protocol = \"https\"\nurl = \"https://example.invalid/claude\"\n\
                       payload = \"raw-binary\"\nentry = \"claude\"\nasset = \"claude-arm64\"\n\
                       sha256 = \"d\"\ntree_root = \"abc\"\nsize = 1\n";

    let dir = scratch("verify-pkg-post-verify");
    let (roster_bytes, roster_sig) = testkit::published_roster();
    let roster = dir.join("aterm-machines.toml");
    let roster_sig_path = dir.join("aterm-machines.toml.sig");
    std::fs::write(&roster, roster_bytes.as_slice()).unwrap();
    std::fs::write(&roster_sig_path, roster_sig.as_slice()).unwrap();
    let master = pk(&ROOT_SEED);

    // The verb, over a manifest MACHINE-SIGNED through the real chain every time.
    let run = |label: &str, body: &str| -> String {
        let pkg = dir.join(format!("pkg-claude-{label}.toml"));
        let sig = dir.join(format!("pkg-claude-{label}.toml.sig"));
        std::fs::write(&pkg, body.as_bytes()).unwrap();
        std::fs::write(&sig, sign(&RELEASE_SEED, body.as_bytes())).unwrap();
        format!(
            "{:?}",
            super::cmd_verify_pkg(&[
                master.clone(),
                pkg.display().to_string(),
                sig.display().to_string(),
                roster.display().to_string(),
                roster_sig_path.display().to_string(),
            ])
        )
    };
    let verified_ok = format!("{:?}", ExitCode::SUCCESS);
    let refused = format!("{:?}", ExitCode::from(1));

    // A manifest a client can read passes, by exactly the signature it always did.
    assert_eq!(
        run("good", &format!("{HEAD}{ROW}")),
        verified_ok,
        "a signed, parsable manifest must still verify"
    );

    // ...and every post-verify gate a client applies now fails the PIPELINE instead.
    for (label, body, names) in [
        (
            "shim-env",
            format!("{HEAD}shim_env = [\"disable_autoupdater=1\"]\n{ROW}"),
            "name is not [A-Z0-9_]+",
        ),
        (
            "no-sha256",
            format!("{HEAD}{}", ROW.replace("sha256 = \"d\"\n", "")),
            "malformed",
        ),
        (
            "retired-kind",
            format!(
                "{HEAD}{}",
                ROW.replace("kind = \"binary\"", "kind = \"vendor-fetch\"")
            ),
            "vendor-fetch",
        ),
        (
            "newer-schema",
            format!("{HEAD}{ROW}").replace("schema = 2", "schema = 3"),
            "schema",
        ),
    ] {
        assert_eq!(
            run(label, &body),
            refused,
            "{label}: a signed manifest no client can parse must not self-check OK"
        );
        let why = super::pkg_post_verify_refusal(&testkit::machine_signed(body.into_bytes()))
            .unwrap_or_else(|| panic!("{label}: parsed, so the verb had nothing to refuse"));
        assert!(
            why.contains(names),
            "{label}: the refusal must name the fix, got {why:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
/// A file `verify-index` / `verify-pkg` cannot read stops it at exit 1 (the path and
/// the error are said), never the usage error's 2.
#[test]
fn verify_stops_at_the_first_unreadable_file() {
    let dir = scratch("verify-unreadable");
    let args: Vec<String> = ["k", "doc", "doc.sig", "roster", "roster.sig"]
        .iter()
        .map(|n| dir.join(n).display().to_string())
        .collect();
    assert!(matches!(
        super::admit_for_verify(&args, "verify-index"),
        Err(code) if code == ExitCode::from(1)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `verify-index` MUST NOT REPORT AN AUTHORING MISTAKE AS A SIGNATURE FAILURE.
///
/// `TrustedRoster::authorize_index` verifies Ed25519 FIRST, then parses the index, then
/// binds its attribution — so `Malformed`, `Schema`, `Unattributed` and `SeqMismatch`
/// are only reachable once a machine on the roster HAS signed these exact bytes. The
/// verb reported every one of them as "no machine on that roster signed this index",
/// which points the publisher at the paper master — re-join, re-key — when the fix is
/// one line of the index they just authored. `tools/atpkg-index.sh` even promises that
/// a misread `roster_seq` "comes back as `Reject::SeqMismatch`", a name the verb never
/// printed.
///
/// Every body below is signed by the roster's OWN machine through the real chain, so
/// the only thing under test is what the verb says once the signature holds — and the
/// last case guards the other direction: a crypto verdict stays opaque (§8).
#[test]
fn verify_index_names_the_post_verify_mistake_instead_of_blaming_the_signature() {
    let dir = scratch("verify-index-post-verify");
    let (roster_bytes, roster_sig) = testkit::published_roster();
    let roster = dir.join("aterm-machines.toml");
    let roster_sig_path = dir.join("aterm-machines.toml.sig");
    std::fs::write(&roster, roster_bytes.as_slice()).unwrap();
    std::fs::write(&roster_sig_path, roster_sig.as_slice()).unwrap();
    let master = pk(&ROOT_SEED);

    // An index this chain accepts: attributed to the roster's machine, at its seq.
    let good = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n[programs.ay]\nrepo = \"ay\"\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );

    // The verb itself, over an index signed by `seed`.
    let run = |label: &str, body: &str, seed: &[u8; 32]| -> String {
        let index = dir.join(format!("index-{label}.toml"));
        let sig = dir.join(format!("index-{label}.toml.sig"));
        std::fs::write(&index, body.as_bytes()).unwrap();
        std::fs::write(&sig, sign(seed, body.as_bytes())).unwrap();
        format!(
            "{:?}",
            super::cmd_verify_index(&[
                master.clone(),
                index.display().to_string(),
                sig.display().to_string(),
                roster.display().to_string(),
                roster_sig_path.display().to_string(),
            ])
        )
    };
    // What the verb PRINTS for those bytes: the refusal the same call hands it.
    let refusal = |body: &str, seed: &[u8; 32]| -> Option<String> {
        let sig = sign(seed, body.as_bytes());
        let Err(why) = testkit::trusted_roster().authorize_index(body.as_bytes().to_vec(), &sig)
        else {
            panic!("this fixture must be refused, or it proves nothing");
        };
        super::index_post_verify_refusal(&why)
    };
    let verified_ok = format!("{:?}", ExitCode::SUCCESS);
    let refused = format!("{:?}", ExitCode::from(1));

    // A correctly authored index still verifies, by exactly the chain it always did.
    assert_eq!(
        run("good", &good, &RELEASE_SEED),
        verified_ok,
        "a signed, parsable, correctly attributed index must still verify"
    );

    for (label, body, names) in [
        // The two an edit to the index authoring script produces.
        (
            "duplicate-key",
            good.replace("schema = 2\n", "schema = 2\nindex_build = 41\n"),
            "malformed",
        ),
        (
            "wrong-typed",
            good.replace("index_build = 41", "index_build = \"41\""),
            "malformed",
        ),
        (
            "newer-schema",
            good.replace("schema = 2", "schema = 3"),
            "schema",
        ),
        (
            "unattributed",
            good.replace(&format!("machine_id = \"{}\"\n", testkit::MACHINE_ID), ""),
            "machine_id",
        ),
        // `tools/atpkg-index.sh` reads `roster_seq` with `sed … | head -n1`; a misread
        // one lands here, and that script promises this refusal is named.
        (
            "seq-mismatch",
            good.replace(
                &format!("roster_seq = {}", testkit::SEQ),
                &format!("roster_seq = {}", testkit::SEQ + 1),
            ),
            "roster_seq",
        ),
    ] {
        assert_eq!(
            run(label, &body, &RELEASE_SEED),
            refused,
            "{label}: an index no client can read must not self-check OK"
        );
        let why = refusal(&body, &RELEASE_SEED).unwrap_or_else(|| {
            panic!("{label}: reported as \"no machine on that roster signed this index\"")
        });
        assert!(
            why.contains(names),
            "{label}: the refusal must name the fix, got {why:?}"
        );
        assert!(
            why.starts_with("the index verified"),
            "{label}: the signature HELD, so the refusal must not read as a signature \
             failure, got {why:?}"
        );
    }

    // THE OTHER DIRECTION, unchanged: a signature that does not verify says nothing
    // beyond that, so the operator still gets the one opaque line (§8).
    assert_eq!(
        run("outsider", &good, &testkit::OUTSIDER_SEED),
        refused,
        "an index signed off-roster must still be refused"
    );
    assert_eq!(
        refusal(&good, &testkit::OUTSIDER_SEED),
        None,
        "a crypto verdict must stay opaque — no verification oracle"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the vendor-direct lane in the pass (design §1.5, §1.8) ----

fn vendor_floor() -> crate::sig::BuildFloor {
    crate::sig::BuildFloor {
        index_build: 0,
        roster_seq: crate::sig::testkit::SEQ,
    }
}

fn vendor_version(text: &str) -> crate::vendor_direct::Version {
    crate::vendor_direct::Version::parse(text).unwrap()
}

fn world_now() -> i64 {
    crate::vendor_direct::lane::world::NOW
}

fn no_shell_hooks(_: &crate::store::Layout) {}

/// No rustup seam: the real one writes the real `~/.rustup`.
fn no_rustup_seam(_: &crate::store::Layout) {}

thread_local! {
    /// Every program a pass on this thread asked to warm, in order ([`record_warm`]).
    static WARMED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The world's warm: recorded on this thread (tests run one per thread), never run.
fn record_warm(_: &crate::store::Layout, programs: &std::collections::BTreeSet<String>) {
    WARMED.with(|w| w.borrow_mut().extend(programs.iter().cloned()));
}

/// What this thread's passes asked to warm since the last call.
fn take_warmed() -> Vec<String> {
    WARMED.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

fn world_anchor(_: &crate::store::Layout) -> crate::Anchor {
    crate::sig::testkit::anchor()
}

/// A process table with nothing in it: the pass tests' flip gate is `Now`, which never
/// reads one, and a test that asks for quiet says so with its own.
fn no_processes() -> Option<Vec<std::path::PathBuf>> {
    Some(Vec::new())
}

/// What a pass runs under here: the test anchor and key, the world's clock, no shell
/// hooks (they write the real home), no progress file, and a warm that only records.
fn world_seams(trust: &crate::vendor_direct::lane::Trust) -> PassSeams<'_> {
    PassSeams {
        anchor: world_anchor,
        now: world_now,
        trust,
        shell_hooks: no_shell_hooks,
        rustup_seam: &no_rustup_seam,
        progress: None,
        path: None,
        warm_agents: record_warm,
        translated: || false,
        flip_policy: || crate::quiet::FlipPolicy::Now,
        running: &no_processes,
    }
}

/// A `dir:` registry whose `rustc` tuple is `trust` + `tb`, pinned at trust 4821 and
/// tb 6 by an index still valid at the world's clock, beside both signed pkgs.
fn write_toolchain_registry(dir: &Path) {
    let body = format!(
        "schema = 2\nindex_build = 51\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ trust = 4821, tb = 6 }}\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    );
    std::fs::write(dir.join("index.toml"), body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, body.as_bytes()),
    )
    .unwrap();
    write_roster(dir);
    write_pkg(dir, "trust", 4821);
    write_pkg(dir, "tb", 6);
}

/// ONE FLIP GATE FOR THE WHOLE UNATTENDED PASS. With a build running from trust 4800, the
/// apply lane stages the `rustc` tuple and waits; the set-completion lane of the same
/// pass, finding the tuple's `tb` absent, must not complete it by flipping trust under
/// that build. `tb` arrives with the flip once the toolchain is quiet. The registry is a
/// `dir:` one, so every download is a hard link to the registry's archive.
#[test]
fn a_busy_toolchain_is_not_flipped_by_set_completion_and_lands_whole_when_quiet() {
    let dir = scratch("busy-set-completion");
    write_toolchain_registry(&dir);
    let layout = temp_layout("busy-set-completion");
    make_live(&layout, "trust", 4800);
    let fetcher = crate::DirFetcher::new(dir.clone());
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["claude".to_string(), "codex".to_string()]),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let exe = layout.build_dir("trust", 4800).join("bin/trust");
    let busy = move || Some(vec![exe.clone()]);
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.flip_policy = || crate::quiet::FlipPolicy::WhenQuiet;
    seams.running = &busy;
    let pass = |seams: &PassSeams<'_>| {
        let code = run_update_pass(
            &layout,
            &cfg,
            &fetcher,
            &crate::active_builds(&layout),
            true,
            seams,
        );
        let live = crate::active_builds(&layout);
        (code, live.get("trust").copied(), live.get("tb").copied())
    };
    assert_eq!(
        pass(&seams),
        (0, Some(4800), None),
        "nothing flips under the build"
    );
    assert!(
        crate::quiet::read(&layout).is_some(),
        "the wait is recorded"
    );
    seams.running = &no_processes;
    assert_eq!(
        pass(&seams),
        (0, Some(4821), Some(6)),
        "the quiet pass lands the tuple whole"
    );
    assert_eq!(crate::quiet::read(&layout), None);
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PASS THAT ONLY APPLIES (no set completion: `auto_install = false`, a machine nothing
/// adopted) and flips trust 4800 to 4821 leaves the rustup view presenting 4821, not the
/// build it moved off.
#[test]
fn an_apply_only_pass_that_flips_trust_re_lays_the_rustup_view() {
    let dir = scratch("apply-only-seam");
    write_toolchain_registry(&dir);
    let layout = temp_layout("apply-only-seam");
    make_live(&layout, "trust", 4800);
    // Bytes of its own, so a view of 4800 cannot pass for one of 4821.
    std::fs::write(
        layout.build_dir("trust", 4800).join("bin/trust"),
        b"#!/bin/true\n# build 4800\n",
    )
    .unwrap();
    let rustup = scratch("apply-only-seam-rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    let _ = crate::seam::reassert(&layout, &rustup);
    let seam = |layout: &crate::store::Layout| {
        let _ = crate::seam::reassert(layout, &rustup);
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.rustup_seam = &seam;
    let code = run_update_pass(
        &layout,
        &crate::config::PackagesConfig::default(),
        &crate::DirFetcher::new(dir.clone()),
        &crate::active_builds(&layout),
        false,
        &seams,
    );
    assert_eq!(
        (code, crate::active_builds(&layout).get("trust").copied()),
        (0, Some(4821))
    );
    assert_eq!(
        crate::seam::first_mismatch(
            &layout.build_dir("trust", 4821),
            &crate::seam::view_dir(&layout, "trust"),
            crate::seam::Depth::Deep
        ),
        None,
        "the view presents the build the pass flipped to"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&rustup);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pause exactly at the vendor release-head GET. The test thread can inspect the
/// real active shim while the pass is unable to finish that unrelated request.
struct BlockVendorHead {
    inner: crate::DirFetcher,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl crate::flow::Fetcher for BlockVendorHead {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        crate::flow::Fetcher::index_candidates(&self.inner)
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        crate::flow::Fetcher::pkg_manifest(&self.inner, repo, program, build)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        crate::flow::Fetcher::download(&self.inner, repo, asset, dest)
    }

    fn vendor_head(
        &self,
        program: &str,
        _: &str,
        _: u64,
        _: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        assert_eq!(program, "claude", "only the installed vendor is checked");
        self.entered.send(()).expect("test observes the vendor GET");
        self.release
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("test releases the vendor GET");
        Err(crate::flow::VendorFetchError::Unreachable(
            "simulated slow vendor head".into(),
        ))
    }

    fn source_id(&self) -> String {
        crate::flow::Fetcher::source_id(&self.inner)
    }
}

/// A window hint advances the genuinely signed, installed ay build before an
/// unrelated vendor GET can answer. Without the hint, §1.5 still runs the vendor
/// first; a signed build below the hint also runs vendor-first; if the hinted index
/// is refused, vendor work still runs before the pass reports the index failure.
/// These are negative controls for an unconditional reversal or dropped vendors.
#[test]
fn a_signed_published_hint_activates_before_a_blocked_vendor_get() {
    for (label, hint, broken_index, ay_at_vendor, pass_code) in [
        ("hinted", Some(46), false, 18, 0),
        ("ordinary", None, false, 17, 0),
        ("lagging", Some(47), false, 17, 0),
        ("refused", Some(46), true, 17, 1),
    ] {
        let dir = scratch(&format!("priority-registry-{label}"));
        write_registry_at(&dir, "stable", 46);
        // The shared registry fixture is intentionally expired for other tests.
        // Re-sign a future-dated index so this test exercises a live selection.
        let body = std::fs::read_to_string(dir.join("index.toml"))
            .unwrap()
            .replace("2026-07-05T12:00:00Z", "2099-01-01T00:00:00Z");
        std::fs::write(dir.join("index.toml"), body.as_bytes()).unwrap();
        std::fs::write(
            dir.join("index.toml.sig"),
            if broken_index {
                b"refused".to_vec()
            } else {
                sign(&RELEASE_SEED, body.as_bytes())
            },
        )
        .unwrap();
        let layout = temp_layout(&format!("priority-store-{label}"));
        make_live(&layout, "ay", 17);
        make_live(&layout, "claude", vendor_version("2.1.281").build_id());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread_layout = layout.clone();
        let worker = std::thread::spawn(move || {
            PUBLISHED_INDEX_HINT.with(|cell| cell.set(hint));
            let fetcher = BlockVendorHead {
                inner: crate::DirFetcher::new(dir.clone()),
                entered: entered_tx,
                release: release_rx,
            };
            let cfg = crate::config::PackagesConfig::default();
            let trust = crate::vendor_direct::lane::world::trust();
            let installed = crate::active_builds(&thread_layout);
            let code = run_update_pass(
                &thread_layout,
                &cfg,
                &fetcher,
                &installed,
                false,
                &world_seams(&trust),
            );
            PUBLISHED_INDEX_HINT.with(|cell| cell.set(None));
            let _ = std::fs::remove_dir_all(&dir);
            code
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the vendor GET is reached");
        let observed = crate::active_builds(&layout).get("ay").copied();
        release_tx.send(()).expect("resume the vendor GET");
        let code = worker.join().expect("the update pass exits");
        assert_eq!(observed, Some(ay_at_vendor), "{label}: at vendor GET");
        assert_eq!(code, pass_code, "{label}: pass verdict");
        if !broken_index {
            assert_eq!(crate::active_builds(&layout).get("ay"), Some(&18));
        }
    }
}

/// THE STALL THIS LANE EXISTS FOR: the vendors' heads move while the ALab index is
/// STALE — and while NO index verifies at all — and the pass's vendor lane installs
/// and then upgrades both, ahead of and apart from the index lane, which still
/// refuses every ALab program on the same fetcher.
#[test]
fn a_moved_head_installs_while_the_index_is_stale_or_absent_and_alab_stays_refused() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let stale = world::signed_index(44, "2026-01-01T00:00:00Z", &[]);
    for (label, index) in [("stale", Some(stale)), ("absent", None)] {
        let layout = world::layout(&format!("pass-{label}"));
        let f = Fake::default();
        *f.index.borrow_mut() = index;
        f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
        f.publish_codex_default("0.157.0");
        let cfg = crate::config::PackagesConfig::default();
        let lanes = VendorLanes {
            update_installed: true,
            complete_the_set: true,
        };
        let run = |installed: &std::collections::BTreeMap<String, u64>| {
            vendor_pass(
                &layout,
                &f,
                &cfg,
                installed,
                lanes,
                Consent::Explicit,
                None,
                &world_seams(&world::trust()),
            )
        };
        let pass = run(&std::collections::BTreeMap::new());
        assert_eq!(pass.failures, 0, "{label}");
        assert!(pass.changed);
        let active = crate::active_builds(&layout);
        assert_eq!(
            active.get("claude").copied(),
            Some(vendor_version("2.1.281").build_id()),
            "{label}"
        );
        assert_eq!(
            active.get("codex").copied(),
            Some(vendor_version("0.157.0").build_id()),
            "{label}"
        );
        f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
        let pass = run(&active);
        assert_eq!(pass.failures, 0, "{label}");
        assert_eq!(
            crate::active_builds(&layout).get("claude").copied(),
            Some(vendor_version("2.1.282").build_id()),
            "{label}: the upgrade landed with no index"
        );
        let rows = crate::status::read(&layout).unwrap().programs;
        assert_eq!(rows["claude"].state, "managed 2.1.282 — Anthropic latest");
        assert_eq!(rows["codex"].state, "managed 0.157.0 — OpenAI latest");
        assert!(!rows["claude"].tree_root.is_empty());
        // The index lane, on the same fetcher, refuses every ALab program.
        let err = crate::flow::install(
            &f,
            &layout,
            &crate::sig::testkit::anchor(),
            &crate::InstallRequest {
                channel: "stable",
                program: "ay",
                triple: crate::vendor_direct::lane::world::triple(),
                installed: None,
            },
            vendor_floor(),
            world::NOW,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                crate::FlowError::Stale | crate::FlowError::Unreachable(_)
            ),
            "{label}: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}

/// THE WINDOW'S HEAD-WATCH PASS (design §1.6) is the update pass's own unattended lane
/// for one installed program; a person's `update <program>` is still the explicit door.
/// Only the person's installs what is missing, fails on an unreached vendor, and
/// forgets a refusal.
#[test]
fn the_windows_vendor_update_is_the_unattended_lane_and_a_persons_the_explicit_door() {
    use crate::vendor_direct::lane::{
        Trust,
        world::{self, Fake, native_exe},
    };
    let layout = world::layout("window-door");
    let f = Fake::default();
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let run = |door: VendorDoor, trust: &Trust| {
        update_vendor(&layout, &f, claude, door, &world_seams(trust))
    };
    let active = || crate::active_builds(&layout).get("claude").copied();
    let build = |v: &str| Some(vendor_version(v).build_id());
    let trust = world::trust();
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    assert!(run(VendorDoor::Window, &trust));
    assert_eq!(active(), None, "the window's pass installs nothing");
    assert!(f.downloads.borrow().is_empty());
    assert!(run(VendorDoor::Person, &trust));
    assert_eq!(active(), build("2.1.281"));
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    assert!(run(VendorDoor::Window, &trust));
    assert_eq!(active(), build("2.1.282"), "the moved head landed");
    f.offline.set(true);
    assert!(run(VendorDoor::Window, &trust), "unreached: quiet");
    assert!(!run(VendorDoor::Person, &trust), "unreached: not checked");
    f.offline.set(false);
    // The Apple anchor exists on macOS only.
    if cfg!(target_os = "macos") {
        f.publish_claude("2.1.283", &native_exe("claude 2.1.283"));
        let refusing = world::trust_refusing_signers();
        assert!(
            !run(VendorDoor::Window, &refusing),
            "a refusal fails the pass"
        );
        let downloads = f.downloads.borrow().len();
        assert!(!run(VendorDoor::Window, &refusing));
        assert_eq!(
            f.downloads.borrow().len(),
            downloads,
            "the window's pass keeps the memo: nothing downloaded again"
        );
        assert!(!run(VendorDoor::Person, &refusing));
        assert_eq!(
            f.downloads.borrow().len(),
            downloads + 1,
            "a person's door forgets it"
        );
        assert_eq!(active(), build("2.1.282"));
        // A signature check that did not finish (2026-09-26): quiet for the window —
        // no failure, no "Package update failed" — and nothing moved; a person's door
        // says it did not happen; the next window pass whose check finishes lands it.
        f.publish_claude("2.1.284", &native_exe("claude 2.1.284"));
        let slow = world::trust_timing_out();
        assert!(run(VendorDoor::Window, &slow), "a deferral is quiet");
        assert_eq!(active(), build("2.1.282"));
        assert!(
            !run(VendorDoor::Person, &slow),
            "a person's move did not happen"
        );
        assert!(run(VendorDoor::Window, &trust));
        assert_eq!(
            active(),
            build("2.1.284"),
            "the next finished check lands it"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE WARM OF A LANDED BUILD ([`crate::warm`], 2026-09-23): every lane that LANDS a
/// new claude asks for it to be warmed exactly once — a pass's vendor lane at either of
/// its exits (a fresh install, then an upgrade), the head watch's and a person's
/// `update claude`, and `install claude` — and a lane that lands nothing (current,
/// unreached, refused) asks for nothing.
#[test]
fn a_landed_agent_build_is_warmed_once_by_every_lane_that_lands_it() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("warm-lanes");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["codex".into()]),
        ..Default::default()
    };
    let trust = world::trust();
    let seams = world_seams(&trust);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let active = || crate::active_builds(&layout).get("claude").copied();
    let build = |v: &str| Some(vendor_version(v).build_id());
    let _ = take_warmed();
    // The pass's vendor lane, completing the set: the fresh-landing exit.
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    let pass = vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    assert_eq!(active(), build("2.1.281"));
    // A fresh install asks too: whether the account ever ran claude — a default-set
    // install nobody launched has nothing to refresh — is the warm's own question
    // (`warm::declined`), answered where the user's config home is known.
    assert_eq!(take_warmed(), ["claude"], "a fresh install");
    assert!(pass.landed.is_empty(), "warmed, then forgotten");
    // The full update pass: an upgrade, at the vendor lane's early exit.
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    let installed = crate::active_builds(&layout);
    run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert_eq!(active(), build("2.1.282"));
    assert_eq!(take_warmed(), ["claude"], "an upgrade");
    let installed = crate::active_builds(&layout);
    run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert!(take_warmed().is_empty(), "current: nothing landed");
    f.offline.set(true);
    run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert!(take_warmed().is_empty(), "unreached: nothing landed");
    f.offline.set(false);
    // The head watch's door, then a person's.
    f.publish_claude("2.1.283", &native_exe("claude 2.1.283"));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &seams
    ));
    assert_eq!(active(), build("2.1.283"));
    assert_eq!(take_warmed(), ["claude"], "the head watch's landing");
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &seams
    ));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &seams
    ));
    assert!(take_warmed().is_empty(), "current, at either door");
    f.publish_claude("2.1.284", &native_exe("claude 2.1.284"));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &seams
    ));
    assert_eq!(take_warmed(), ["claude"], "a person's landing");
    // A refusal lands nothing (the Apple anchor exists on macOS only).
    if cfg!(target_os = "macos") {
        f.publish_claude("2.1.285", &native_exe("claude 2.1.285"));
        let refusing = world::trust_refusing_signers();
        let refused = PassSeams {
            trust: &refusing,
            ..world_seams(&trust)
        };
        assert!(!update_vendor(
            &layout,
            &f,
            claude,
            VendorDoor::Window,
            &refused
        ));
        assert_eq!(active(), build("2.1.284"));
        assert!(take_warmed().is_empty(), "refused: nothing landed");
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    // `install claude`.
    let layout = world::layout("warm-install-door");
    install_vendor_door(&layout, &f, "stable", claude, &seams).expect("installs");
    assert!(crate::active_builds(&layout).contains_key("claude"));
    assert_eq!(take_warmed(), ["claude"], "the install door's landing");
    install_vendor_door(&layout, &f, "stable", claude, &seams).expect("current");
    assert!(take_warmed().is_empty(), "current: nothing landed");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The warm verb is machinery, not vocabulary: no roster row, no usage, no lock.
#[test]
fn the_warm_verb_is_unlisted_and_takes_no_lock() {
    assert!(!dispatch_roster().contains(&crate::warm::HIDDEN_VERB));
    assert!(!verb_mutates_store(crate::warm::HIDDEN_VERB));
    assert_eq!(verb_usage(crate::warm::HIDDEN_VERB), None);
}

/// ONLY AN INSTALL IS A LANDING: a rollback re-activates a build that already ran here,
/// a tombstone runs nothing, and every other verdict moved nothing — none is warmed.
#[test]
fn only_an_installed_verdict_is_a_landing() {
    use crate::vendor_direct::decide::{InstallReason, Installed, KeepReason};
    use crate::vendor_direct::lane::{Outcome, Verdict};
    let layout = crate::vendor_direct::lane::world::layout("warm-verdicts");
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let v = vendor_version("2.1.280");
    let verdicts = [
        (
            "installed",
            Verdict::Installed {
                from: Installed::Nothing,
                to: v,
                reason: InstallReason::Fresh,
                shimmed: vec!["claude".into()],
                refused_shims: Vec::new(),
                tree_root: "b".repeat(64),
            },
            true,
        ),
        ("current", Verdict::Current(v), false),
        (
            "kept",
            Verdict::Kept {
                reason: KeepReason::Held,
                head: Some(v),
                keeping: Installed::Nothing,
            },
            false,
        ),
        (
            "unreachable",
            Verdict::Unreachable {
                keeping: Installed::Nothing,
                why: None,
            },
            false,
        ),
        (
            "refused",
            Verdict::Refused {
                version: Some(v),
                why: "digest mismatch".into(),
                keeping: Installed::Nothing,
            },
            false,
        ),
        (
            "rolled back",
            Verdict::RolledBack {
                from: Installed::Nothing,
                to: v,
            },
            false,
        ),
        (
            "tombstoned",
            Verdict::Tombstoned {
                from: Installed::Nothing,
            },
            false,
        ),
        (
            "failed",
            Verdict::Failed {
                version: v,
                error: "disk full".into(),
                keeping: Installed::Nothing,
            },
            false,
        ),
        ("linked", Verdict::Linked, false),
    ];
    for (label, verdict, lands) in verdicts {
        let outcome = Outcome {
            spec: claude,
            verdict,
            asset: None,
        };
        let mut pass = VendorPass::default();
        report_vendor(&layout, &outcome, &mut pass, false, world_now());
        assert_eq!(pass.landed.contains("claude"), lands, "{label}: a pass");
        let mut door = VendorPass::default();
        note_landed(&mut door, &outcome);
        assert_eq!(door.landed.contains("claude"), lands, "{label}: a door");
        let trust = crate::vendor_direct::lane::world::trust();
        let _ = take_warmed();
        warm_landed(&layout, &mut pass, &world_seams(&trust));
        assert_eq!(!take_warmed().is_empty(), lands, "{label}: warmed");
        assert!(pass.landed.is_empty());
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// R5 (2026-09-23): AN OLD AUTO-VENDOR LANE'S RE-PIN NEVER DOWNGRADES OR FIGHTS. The
/// index pins claude at a legacy build (what `tools/atpkg-auto-vendor.sh`, still armed on
/// a machine this repo cannot reach, keeps publishing), naming a version BEHIND the
/// vendor's head: pass after pass the vendor build stays — nothing is fetched for the pin
/// and the store never flips back (a shared store's old client and new client used to
/// take turns). And where a pin build IS installed — an older client on a shared store
/// laid it — it is a floor: a head older than it keeps it (never an automatic
/// downgrade), and the first head past it replaces it. So the machine holds the newer of
/// the pin it has and the vendor's head. The pin's manifest carries an artifact for this
/// triple and the fake's release lane panics, so a pass that planned the pin would fail
/// here (mutation-checked: without `apply::plan_groups`' vendor skip it does). The
/// control: the same passes over the same index move claude as soon as the head does.
#[test]
fn an_index_re_pin_of_a_vendor_program_never_downgrades_or_fights() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let pin = 2_026_091_901;
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    // A lagging re-pin under a vendor build.
    let layout = world::layout("repin-lagging");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg_with_artifact("claude", pin, "2.1.280");
    f.publish_claude("2.1.281", &native_exe("vendor claude"));
    make_live(&layout, "ay", 18);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let active =
        |layout: &crate::store::Layout| crate::active_builds(layout).get("claude").copied();
    let head = vendor_version("2.1.281").build_id();
    assert_eq!(active(&layout), Some(head));
    for pass in 0..2 {
        let installed = crate::active_builds(&layout);
        let _ = run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
        assert_eq!(
            active(&layout),
            Some(head),
            "pass {pass}: no downgrade, no flip"
        );
    }
    assert!(
        f.downloads
            .borrow()
            .iter()
            .all(|url| !url.contains(&pin.to_string())),
        "nothing is fetched for the pin: {:?}",
        f.downloads.borrow()
    );
    // A pin build an older client laid, newer than the vendor's head: kept (a floor),
    // until a head passes it.
    let layout = world::layout("repin-floor");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg_with_artifact("claude", pin, "2.1.285");
    f.publish_claude("2.1.281", &native_exe("an older head"));
    make_live(&layout, "claude", pin);
    make_live(&layout, "ay", 18);
    let installed = crate::active_builds(&layout);
    let _ = run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert_eq!(
        active(&layout),
        Some(pin),
        "a head older than the installed pin never replaces it"
    );
    f.publish_claude("2.1.286", &native_exe("a newer head"));
    let installed = crate::active_builds(&layout);
    let _ = run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert_eq!(
        active(&layout),
        Some(vendor_version("2.1.286").build_id()),
        "the control: the first head past the pin replaces it"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// NO INDEX RULE DECIDES A VENDOR PROGRAM: a fresh index pinning a YANKED legacy claude
/// leaves the vendor build and its shims untouched through the channel apply; the
/// single-program planners refuse it by name; an install of it forks to the vendor
/// lane before the index resolve; and the index's yank is latched for the vendor lane.
#[test]
fn a_yanked_legacy_pin_never_reaches_a_vendor_build() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("legacy-pin");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(
        44,
        "2099-01-01T00:00:00Z",
        &["claude@2026091901", "codex@2026091901"],
    ));
    f.publish_claude("2.1.281", &native_exe("vendor claude"));
    let cfg = crate::config::PackagesConfig::default();
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &world_seams(&world::trust()),
    );
    let claude = crate::store::ToolName::new("claude").unwrap();
    let shim = std::fs::read(layout.shim(&claude)).unwrap();
    let build = vendor_version("2.1.281").build_id();
    let installed = crate::active_builds(&layout);
    assert_eq!(installed.get("claude").copied(), Some(build));
    let triple = crate::vendor_direct::lane::world::triple();
    let anchor = crate::sig::testkit::anchor();
    let report = crate::flow::apply_channel(
        &f,
        &layout,
        &anchor,
        "stable",
        triple,
        &installed,
        &[],
        vendor_floor(),
        world::NOW,
    )
    .expect("the index verifies");
    assert!(
        report
            .groups
            .iter()
            .all(|(g, _)| g.members.iter().all(|m| m == "ay")),
        "{:?}",
        report.groups
    );
    assert_eq!(
        std::fs::read(layout.shim(&claude)).unwrap(),
        shim,
        "not tombstoned"
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(build)
    );
    for planned in [
        crate::flow::plan_update(
            &f,
            &layout,
            &anchor,
            "stable",
            triple,
            "claude",
            Some(build),
            vendor_floor(),
            world::NOW,
        )
        .map(|_| ()),
        crate::flow::apply_program(
            &f,
            &layout,
            &anchor,
            "stable",
            triple,
            "claude",
            &installed,
            &[],
            vendor_floor(),
            world::NOW,
        )
        .map(|_| ()),
    ] {
        match planned {
            Err(crate::FlowError::Vendor(why)) => {
                assert!(why.contains("not through the ALab index"), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }
    // An install forks before the index resolve: under the compiled Anthropic key the
    // test-signed 2.1.282 is the VENDOR lane's refusal, never an index decision.
    f.publish_claude("2.1.282", &native_exe("next"));
    let err = crate::flow::install(
        &f,
        &layout,
        &anchor,
        &crate::InstallRequest {
            channel: "stable",
            program: "claude",
            triple,
            installed: Some(build),
        },
        vendor_floor(),
        world::NOW,
    )
    .unwrap_err();
    match err {
        crate::FlowError::Vendor(line) => {
            assert!(line.starts_with("claude 2.1.282 refused: "), "{line}");
            assert!(line.ends_with("keeping 2.1.281"), "{line}");
        }
        other => panic!("{other:?}"),
    }
    // The fresh index's yanks were latched for the vendor lane.
    let latch = crate::vendor_direct::policy::YankLatch::read(&layout).unwrap();
    assert_eq!(latch.index_build, 44);
    assert!(latch.policy.is_legacy_yanked("claude", 2_026_091_901));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A YANK SURVIVES STALENESS: a fresh index's version yank is latched; with the index
/// stale the next pass still refuses to move to the yanked head.
#[test]
fn a_latched_yank_binds_the_vendor_lane_while_the_index_is_stale() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("latched");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    crate::resolve_verified_index(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        vendor_floor(),
        world::NOW,
    )
    .expect("fresh");
    *f.index.borrow_mut() = Some(world::signed_index(46, "2026-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    f.publish_codex_default("0.157.0");
    let cfg = crate::config::PackagesConfig::default();
    let lanes = VendorLanes {
        update_installed: true,
        complete_the_set: true,
    };
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        lanes,
        Consent::Explicit,
        None,
        &world_seams(&world::trust()),
    );
    f.publish_claude("2.1.282", &native_exe("b"));
    let pass = vendor_pass(
        &layout,
        &f,
        &cfg,
        &crate::active_builds(&layout),
        lanes,
        Consent::Explicit,
        None,
        &world_seams(&world::trust()),
    );
    assert_eq!(pass.failures, 0);
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.281").build_id()),
        "the yanked head was skipped under the latch"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// Two hours after the world's clock: a latch verified at [`world_now`] is past its hour.
fn two_hours_on() -> i64 {
    world_now() + 2 * 60 * 60
}

/// Half an hour after [`two_hours_on`]: a latch verified then is still current.
fn two_and_a_half_hours_on() -> i64 {
    two_hours_on() + 30 * 60
}

/// A YANK PUBLISHED SINCE THE LAST PASS BINDS THE HEAD WATCH'S DOOR (design §1.4–§1.5):
/// the latch the last verification left is over an hour old, so the `--head-watch`
/// door verifies the index first and skips the head the newer index yanks. Within the
/// hour after that verification a door decides on the latch without asking the index.
#[test]
fn a_yank_published_after_the_last_pass_binds_the_head_watch_door() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::policy::YankLatch;
    let layout = world::layout("door-yank");
    let f = Fake::default();
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let trust = world::trust();
    let active = || crate::active_builds(&layout).get("claude").copied();
    // The last pass: index 44, no yanks, verified now; a person installs 2.1.281.
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &world_seams(&trust)
    ));
    assert_eq!(active(), Some(vendor_version("2.1.281").build_id()));
    assert_eq!(
        YankLatch::read(&layout).map(|l| (l.index_build, l.checked_at)),
        Some((44, world_now())),
        "the door's own verification wrote the latch"
    );
    // Two hours on, index 45 yanks the head the vendor has just published.
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    let later = PassSeams {
        now: two_hours_on,
        ..world_seams(&trust)
    };
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &later
    ));
    assert_eq!(
        active(),
        Some(vendor_version("2.1.281").build_id()),
        "the yank published after the last pass bound the head watch's door"
    );
    assert!(f.downloads.borrow().iter().all(|u| !u.contains("2.1.282")));
    let latch = YankLatch::read(&layout).unwrap();
    assert_eq!((latch.index_build, latch.checked_at), (45, two_hours_on()));
    let row = &crate::status::read(&layout).unwrap().programs["claude"];
    assert_eq!(
        row.state,
        "managed 2.1.281 — Anthropic's latest (2.1.282) is yanked"
    );
    // Within the hour: no index read, and the refreshed latch still binds.
    let reads = f.index_reads.get();
    let soon = PassSeams {
        now: two_and_a_half_hours_on,
        ..world_seams(&trust)
    };
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &soon
    ));
    assert_eq!(
        f.index_reads.get(),
        reads,
        "a current latch spares the resolve"
    );
    assert_eq!(active(), Some(vendor_version("2.1.281").build_id()));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A YANK PUBLISHED SINCE THE LAST PASS BINDS A FIRST LAUNCH'S INSTALL: the consented
/// `install claude` a pending stub re-enters ([`install_vendor_door`]), and the flow's
/// own install fork, find no latch, verify the index first, and never install the
/// version it yanks.
#[test]
fn a_yank_published_after_the_last_pass_binds_a_first_launch_install() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let trust = world::trust();
    let yanked = |err: crate::FlowError| match err {
        crate::FlowError::Vendor(line) => assert_eq!(
            line,
            "claude — Anthropic's latest (2.1.282) is yanked; nothing is installed yet"
        ),
        other => panic!("{other:?}"),
    };
    let layout = world::layout("first-launch-yank");
    yanked(install_vendor_door(&layout, &f, "stable", claude, &world_seams(&trust)).unwrap_err());
    assert_eq!(crate::active_builds(&layout).get("claude"), None);
    let layout = world::layout("first-launch-yank-flow");
    yanked(
        crate::flow::install(
            &f,
            &layout,
            &crate::sig::testkit::anchor(),
            &crate::InstallRequest {
                channel: "stable",
                program: "claude",
                triple: world::triple(),
                installed: None,
            },
            vendor_floor(),
            world::NOW,
        )
        .unwrap_err(),
    );
    assert_eq!(crate::active_builds(&layout).get("claude"), None);
    assert!(f.downloads.borrow().is_empty(), "nothing fetched");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE DOOR'S RESOLVE NEVER BLOCKS IT: with the latch a day old and no index that
/// verifies — none published, or only a stale one — `install claude`, the head watch's
/// door and a person's `update claude` each try the index, then decide on the latch
/// (whose yank still binds) and land every admissible head.
#[test]
fn a_failing_resolve_never_blocks_a_vendor_door() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let stale = world::signed_index(46, "2026-01-01T00:00:00Z", &[]);
    for (label, index) in [("absent", None), ("stale", Some(stale))] {
        let layout = world::layout(&format!("door-no-index-{label}"));
        let f = Fake::default();
        *f.index.borrow_mut() = Some(world::signed_index(
            45,
            "2099-01-01T00:00:00Z",
            &["claude@2.1.282"],
        ));
        crate::resolve_verified_index(
            &f,
            &layout,
            &crate::sig::testkit::anchor(),
            vendor_floor(),
            world_now() - 24 * 60 * 60,
        )
        .expect("the last pass verified the index a day ago");
        *f.index.borrow_mut() = index;
        let claude = crate::vendor_direct::spec("claude").unwrap();
        let trust = world::trust();
        let seams = world_seams(&trust);
        let active = || crate::active_builds(&layout).get("claude").copied();
        let tried = |before: u32| {
            assert!(
                f.index_reads.get() > before,
                "{label}: the door tried the index"
            );
            f.index_reads.get()
        };
        f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
        let reads = f.index_reads.get();
        install_vendor_door(&layout, &f, "stable", claude, &seams).expect(label);
        let reads = tried(reads);
        assert_eq!(active(), Some(vendor_version("2.1.281").build_id()));
        f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
        assert!(update_vendor(
            &layout,
            &f,
            claude,
            VendorDoor::Window,
            &seams
        ));
        let reads = tried(reads);
        assert_eq!(
            active(),
            Some(vendor_version("2.1.281").build_id()),
            "{label}: the latch's yank still binds"
        );
        f.publish_claude("2.1.283", &native_exe("claude 2.1.283"));
        assert!(update_vendor(
            &layout,
            &f,
            claude,
            VendorDoor::Person,
            &seams
        ));
        tried(reads);
        assert_eq!(active(), Some(vendor_version("2.1.283").build_id()));
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}

/// A PASS WHOSE INDEX LANE CANNOT RUN still settles what its vendor lane moved: with
/// the index stale across two vendor upgrades, each pass exits 1 and the store keeps
/// the live build and one rollback — never every superseded one.
#[test]
fn a_stale_index_pass_reclaims_what_the_vendor_lane_superseded() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("pass-gc");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2026-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    for (version, tag) in [("2.1.282", "b"), ("2.1.283", "c")] {
        f.publish_claude(version, &native_exe(tag));
        let installed = crate::active_builds(&layout);
        assert_eq!(
            run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
            1,
            "the stale index fails the pass"
        );
        assert_eq!(
            crate::active_builds(&layout).get("claude").copied(),
            Some(vendor_version(version).build_id()),
            "the vendor lane moved ahead of it"
        );
    }
    let mut kept: Vec<u64> = crate::ops::list_installed(&layout)
        .into_iter()
        .filter(|(p, _)| p == "claude")
        .map(|(_, b)| b)
        .collect();
    kept.sort_unstable();
    assert_eq!(
        kept,
        [
            vendor_version("2.1.282").build_id(),
            vendor_version("2.1.283").build_id()
        ],
        "live + 1"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A VENDOR REFUSAL FAILS THE PASS even when the index resolves and applies clean —
/// the index lane's count is added to the vendor lane's, never written over it — and
/// the program's row says so where doctor looks. A head kept at for a reason that is
/// no fault is named, never called the vendor's latest.
#[test]
fn a_vendor_refusal_fails_a_pass_whose_index_is_fresh() {
    use crate::vendor_direct::lane::world::{self, CLAUDE, Fake, native_exe};
    let layout = world::layout("pass-refusal");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let build = vendor_version("2.1.281").build_id();
    // 2.1.282 arrives with a signature that does not verify.
    f.publish_claude("2.1.282", &native_exe("b"));
    let forged = crate::openpgp::testkit::sign_detached(b"another document", 1_790_000_000);
    f.doc(&format!("{CLAUDE}2.1.282/manifest.json.sig"), &forged);
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    let status = crate::status::read(&layout).unwrap();
    let row = &status.programs["claude"];
    assert!(
        row.state.starts_with("error: claude 2.1.282 refused: "),
        "{}",
        row.state
    );
    assert_eq!(row.installed_build, Some(build));
    assert!(!row.tree_root.is_empty(), "the attestation is kept");
    assert!(
        !status.last_success_at.is_empty(),
        "the index lane resolved: a check that ran"
    );
    // The index then yanks 2.1.282; the very next pass — which verifies the index
    // before its vendor lane runs — keeps 2.1.281 and says why.
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(
        row.state,
        "managed 2.1.281 — Anthropic's latest (2.1.282) is yanked"
    );
    assert_eq!(row.installed_build, Some(build));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

thread_local! {
    /// The conformance's pass clock, in ten-second ticks past the world's `NOW`.
    static TICK: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// The world's clock plus [`TICK`] ten-second steps: every pass of the conformance
/// stamps a strictly later second, as the model's `t` does.
fn ticking_now() -> i64 {
    world_now() + 10 * TICK.with(std::cell::Cell::get)
}

/// TIER-1 CONFORMANCE of `AtpkgPassStamps` (aterm-spec): the REAL writer — the whole
/// recorded `update` pass, over the world's fake channel and a real `status.toml` — and
/// the REAL readers (`pkg_check::Stamps::read` and its `last_failed`/`last_attempt`,
/// `pkg_check::healed`, and `pass_ended_since_queued`) step for step against the
/// model, each record projected onto the model's `succ`/`fail` (ten-second ticks past the
/// world's `NOW`; `fail` is the end of a pass recorded as not `ok` with no success after
/// it). The stand-down reader reports how the pass it stood down behind ended; standing
/// down as `ok` is what the model's invariant bounds, so it is checked as an
/// implication, and shown to happen at least once. The trace is the audit's: this lane's pass fails, a sibling's is
/// served from the §14 cache after a refusal, a vendor door writes a row, a pass queues
/// behind the lock, a pass is offline, and one reaches the index.
///
/// NEGATIVE CONTROLS, so the agreement is never vacuous. PK-3: the `Buggy` model's
/// cache-served pass stamps a success, and the real record after the same pass does
/// not — the writer refutes the historical arm. PK-4: the historical reader
/// (`updated_at` after the last success ⇒ failed) is evaluated over the very file the
/// door wrote and says "failed", which the real reader and the model's truth do not.
#[test]
fn the_pass_record_refines_the_derived_model() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use aterm_update_core::pkg_check::{self, Stamps};
    let model = aterm_spec::derive::atpkg_pass_stamps_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let layout = world::layout("pass-record");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = PassSeams {
        now: ticking_now,
        ..world_seams(&trust)
    };
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let installed = crate::active_builds(&layout);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let mut state = model.init_state();
    let mut historical = buggy.init_state();
    let tick = |state: &std::collections::BTreeMap<&'static str, i64>| {
        TICK.with(|t| t.set(state["t"] + 1));
    };
    let at = |t: i64| world_now() + 10 * t;
    let slot = |unix: Option<i64>| unix.map_or(0, |u| (u - world_now()) / 10);
    // The record and every reader against the model's state, after each step.
    let stood_down_ok = std::cell::Cell::new(false);
    // The store's full-pass count when the waiter queued (the model's `Wait`), which the
    // stand-down reader compares with the count it finds under the lock.
    let queued_at: std::cell::Cell<Option<u64>> = std::cell::Cell::new(None);
    let agree = |state: &std::collections::BTreeMap<&'static str, i64>, step: &str| {
        let stamps = Stamps::read(&layout.status());
        let record = crate::status::read(&layout).unwrap_or_default();
        let raw = |stamp: &str| pkg_check::rfc3339_to_unix(stamp.trim());
        let failure = raw(&record.last_pass_at).filter(|&end| {
            record.last_pass != pkg_check::PassOutcome::Ok.word()
                && raw(&record.last_success_at).is_none_or(|ok| ok < end)
        });
        assert_eq!(
            slot(raw(&record.last_success_at)),
            state["succ"],
            "{step}: succ"
        );
        assert_eq!(slot(failure), state["fail"], "{step}: fail");
        assert_eq!(
            stamps.last_failed(),
            state["pass_failed"] == 1,
            "{step}: the failed reader"
        );
        assert_eq!(
            slot(stamps.last_attempt),
            state["pass_end"],
            "{step}: the attempted reader"
        );
        if state["own_fail"] > 0 {
            assert_eq!(
                pkg_check::healed(&stamps, at(state["own_fail"])),
                state["succ"] > state["own_fail"],
                "{step}: the healed reader"
            );
        }
        if state["wait"] > 0 {
            let ok = matches!(
                pass_ended_since_queued(
                    true,
                    queued_at.get(),
                    crate::status::pass_seq(&layout),
                    &stamps,
                    ticking_now(),
                    None,
                ),
                Some((pkg_check::PassOutcome::Ok, _))
            );
            assert!(
                !ok || state["succ"] > state["wait"],
                "{step}: the stand-down reader stood down as ok with nothing reached"
            );
            stood_down_ok.set(stood_down_ok.get() || ok);
        }
        for invariant in [
            "FailedMeansTheLastPassFailed",
            "SpacingCountsOnlyFullPasses",
            "HealedOnlyByAReachedPass",
            "StandDownOnlyBehindAReachedPass",
        ] {
            assert!(
                model.check_invariant(invariant, state),
                "{step}: {invariant}"
            );
        }
    };
    let fire = |m: &aterm_spec::derive::Model,
                state: &mut std::collections::BTreeMap<&'static str, i64>,
                action: &str| {
        assert!(m.fire(action, state), "{action}");
    };

    // 1. A pass that reaches the index: a success, and the cache holds the index now.
    tick(&state);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    fire(&model, &mut state, "PassReached");
    fire(&buggy, &mut historical, "PassReached");
    agree(&state, "reached");

    // 2. THIS LANE's pass is refused (a rate limit): it runs on the §14 cache, exits 1
    //    and stamps a failure — never a success (it used to exit 0 and stamp one).
    *f.index.borrow_mut() = None;
    f.index_refused.set(true);
    tick(&state);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "a pass on the cache is not a check that ran"
    );
    fire(&model, &mut state, "OwnPassUnreached");
    fire(&buggy, &mut historical, "OwnPassUnreached");
    agree(&state, "own pass refused");

    // 3. A pass queues behind the store lock, reading the store's pass count first.
    tick(&state);
    queued_at.set(Some(crate::status::pass_seq(&layout)));
    fire(&model, &mut state, "Wait");
    fire(&buggy, &mut historical, "Wait");
    agree(&state, "waiting");

    // 4. A SIBLING's pass, refused the same way, served from the cache. PK-3's replay:
    //    the historical writer stamped a success here — healing this lane's ladder and
    //    standing the waiter down with nothing reached. The real record did not.
    tick(&state);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    fire(&model, &mut state, "PassUnreached");
    fire(&buggy, &mut historical, "PassUnreached");
    agree(&state, "sibling on the cache");
    assert_ne!(
        slot(Stamps::read(&layout.status()).last_success),
        historical["succ"],
        "the negative control: the historical writer's success is not in the record"
    );
    assert!(!buggy.check_invariant("HealedOnlyByAReachedPass", &historical));
    assert!(!buggy.check_invariant("StandDownOnlyBehindAReachedPass", &historical));

    // 5. The index is served again, then a vendor door writes a row. PK-4's replay:
    //    the historical reader took the door's `updated_at` for a failed pass.
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.index_refused.set(false);
    tick(&state);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    fire(&model, &mut state, "PassReached");
    fire(&buggy, &mut historical, "PassReached");
    agree(&state, "reached again");
    f.publish_claude("2.1.282", &native_exe("b"));
    tick(&state);
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &seams
    ));
    fire(&model, &mut state, "OtherWrite");
    fire(&buggy, &mut historical, "OtherWrite");
    agree(&state, "a vendor door");
    let record = crate::status::read(&layout).unwrap();
    let unix = |stamp: &str| pkg_check::rfc3339_to_unix(stamp).unwrap();
    assert!(
        unix(&record.updated_at) > unix(&record.last_success_at),
        "the door moved the last-write stamp past the success"
    );
    assert!(
        !buggy.check_invariant("FailedMeansTheLastPassFailed", &historical),
        "the negative control: the historical reader calls that a failed pass"
    );

    // 6. Nothing answers — offline: exit 69, a failure stamped.
    *f.index.borrow_mut() = None;
    f.offline.set(true);
    tick(&state);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        pkg_check::PASS_OFFLINE_EXIT
    );
    fire(&model, &mut state, "PassUnreached");
    agree(&state, "offline");
    assert!(
        stood_down_ok.get(),
        "the stand-down reader's ok arm was exercised behind the reached pass"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// OFFLINE IS NOT SUCCESS (Phase 3). A pass whose index came only from the §14 cache
/// and whose vendor channels did not answer checked nothing: it exits
/// `PASS_OFFLINE_EXIT` (not 0, 1 or 75) and leaves `last_success_at` where it was. One
/// that reached either is an ordinary pass; one with no index at all and no vendor
/// reached is offline too, never "update failed".
#[test]
fn a_pass_that_reached_nothing_is_offline_not_a_success() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
    assert!(![0, 1, 2, crate::lock::CONTENDED_EXIT].contains(&OFFLINE));
    let layout = world::layout("pass-offline");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0,
        "online: a check that ran (and the index is cached now)"
    );
    let checked = crate::status::read(&layout).unwrap().last_success_at;
    assert!(!checked.is_empty());
    // Nothing answers: the index (the cache serves it) and the vendor.
    *f.index.borrow_mut() = None;
    f.offline.set(true);
    std::fs::write(
        layout.status(),
        std::fs::read_to_string(layout.status())
            .unwrap()
            .replace(&checked, "2026-09-01T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        OFFLINE
    );
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(
        status.last_success_at, "2026-09-01T00:00:00Z",
        "an offline pass never stamps a success"
    );
    assert!(
        status.outcome.contains("index from cache"),
        "{}",
        status.outcome
    );
    assert_eq!(status.last_pass, "offline", "…it records an offline end");
    assert_eq!(
        aterm_update_core::pkg_check::rfc3339_to_unix(&status.last_pass_at),
        Some(world_now()),
        "…at the pass's own clock"
    );
    // The vendor answers while the index does not: not offline — and still not a check
    // that ran, since the index came from the cache.
    f.offline.set(false);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    // No index anywhere (a first launch before the network came up) and no vendor.
    let bare = world::layout("pass-offline-bare");
    make_live(&bare, "claude", vendor_version("2.1.281").build_id());
    f.offline.set(true);
    assert_eq!(
        run_update_pass(&bare, &cfg, &f, &crate::active_builds(&bare), false, &seams),
        OFFLINE,
        "offline, not failed"
    );
    assert!(crate::status::never_checked(&bare));
    let _ = std::fs::remove_dir_all(&bare.prefix);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A HOST THAT ANSWERED IS NOT AN OFFLINE MACHINE. The index source refused with a
/// status (a rate limit, a renamed repo) while every vendor channel was down: the pass
/// ran on the cached index and exits 1 — a failure the window surfaces, its outcome
/// naming the refusal — never the offline code the window retries quietly. A vendor
/// channel that answered with a status is a host that answered too.
#[test]
fn an_index_host_that_answered_with_a_refusal_is_a_failure_not_offline() {
    use crate::vendor_direct::lane::world::{self, CLAUDE, Fake, native_exe};
    const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
    let layout = world::layout("pass-refused");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    *f.index.borrow_mut() = None;
    f.index_refused.set(true);
    f.offline.set(true);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "a refusal is a failure"
    );
    let outcome = crate::status::read(&layout).unwrap().outcome;
    assert!(outcome.contains("index from cache"), "{outcome}");
    assert!(outcome.contains("HTTP 403"), "{outcome}");
    // The listing's link down, and the vendor's head answering 404: not offline either.
    f.index_refused.set(false);
    f.offline.set(false);
    f.forget(&format!("{CLAUDE}latest"));
    let code = run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert_ne!(code, OFFLINE, "the vendor's host answered");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A PASS THAT FAILED ONLY FOR ITS CACHED INDEX IS RECORDED AS THAT, AND A CHILD THAT
/// STANDS DOWN BEHIND IT EXITS AS IT DID (review of 4329ea590, 2026-09-26). The index
/// listing refused (a rate limit) while no member failed: exit 1, nothing on stderr, and
/// the record says so for THAT pass end ([`crate::status::stale_index_only_end`]) — so a
/// `--wait-lock` child that stood down behind it says its line on stdout and exits 1
/// ([`stand_down_exit`]), which the window reads as a stale index, not "Package update
/// failed". Controls: a clean pass and an offline one are not that; a pass on the
/// cached index that ALSO failed a member (a signer refusal) is not that either, and
/// its stand-down still goes to stderr.
#[test]
fn a_pass_that_failed_only_for_its_cached_index_is_recorded_and_stood_down_as_that() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use aterm_update_core::pkg_check::PassOutcome;
    let layout = world::layout("stale-index-only");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let installed = crate::active_builds(&layout);
    let record = || crate::status::read(&layout).expect("a record");
    let stand_down = |r: &crate::status::Status| {
        stand_down_exit(
            PassOutcome::Failed,
            3,
            &r.outcome,
            crate::status::stale_index_only_end(r),
        )
    };
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    assert!(
        !crate::status::stale_index_only_end(&record()),
        "a clean pass"
    );
    // The listing refuses (HTTP 403): the pass runs on the cache, no member fails.
    *f.index.borrow_mut() = None;
    f.index_refused.set(true);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    let stale = record();
    assert!(
        stale
            .outcome
            .contains(crate::status::INDEX_FROM_CACHE_CLAUSE),
        "{}",
        stale.outcome
    );
    assert!(
        crate::status::stale_index_only_end(&stale),
        "the record names this pass end as failed only for its index: {stale:?}"
    );
    let said = stand_down(&stale);
    assert_eq!(said.code, 1);
    assert!(
        !said.on_stderr,
        "the stand-down exits as that pass did: nothing on stderr ({})",
        said.line
    );
    // Offline: not that.
    f.index_refused.set(false);
    f.offline.set(true);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        aterm_update_core::pkg_check::PASS_OFFLINE_EXIT
    );
    assert!(!crate::status::stale_index_only_end(&record()), "offline");
    f.offline.set(false);
    // CONTROL (the Apple anchor exists on macOS only): the cached index AND a member
    // refused — a failure the stand-down still says on stderr.
    if cfg!(target_os = "macos") {
        f.index_refused.set(true);
        f.publish_claude("2.1.282", &native_exe("b"));
        let refusing = world::trust_refusing_signers();
        let seams = world_seams(&refusing);
        assert_eq!(
            run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
            1
        );
        let both = record();
        assert!(
            both.outcome
                .contains(crate::status::INDEX_FROM_CACHE_CLAUSE),
            "{}",
            both.outcome
        );
        assert!(
            !crate::status::stale_index_only_end(&both),
            "a member failed too: {both:?}"
        );
        assert!(stand_down(&both).on_stderr, "a failure, said as one");
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The stand-down's exit per holder ending ([`stand_down_exit`]): `ok` 0 and `offline`
/// the offline code on stdout; `failed` 1 on stderr — on stdout only for a holder that
/// failed for its cached index alone. The flag never quiets an ok or offline ending's
/// code, nor moves them to stderr.
#[test]
fn a_stand_down_exits_as_the_pass_it_stood_behind() {
    use aterm_update_core::pkg_check::{PASS_OFFLINE_EXIT, PassOutcome};
    for only in [false, true] {
        let ok = stand_down_exit(PassOutcome::Ok, 2, "up to date", only);
        assert_eq!((ok.on_stderr, ok.code), (false, 0));
        let off = stand_down_exit(PassOutcome::Offline, 2, "", only);
        assert_eq!((off.on_stderr, off.code), (false, PASS_OFFLINE_EXIT));
    }
    let failed = stand_down_exit(PassOutcome::Failed, 2, "1 program(s) failed", false);
    assert_eq!((failed.on_stderr, failed.code), (true, 1));
    let stale = stand_down_exit(
        PassOutcome::Failed,
        2,
        "up to date (index build 46) (index from cache \u{2014} GitHub rate limit hit \
         (HTTP 403))",
        true,
    );
    assert_eq!((stale.on_stderr, stale.code), (false, 1));
    assert!(stale.line.contains("index from cache"), "{}", stale.line);
}

/// EVERY FULL PASS RECORDS HOW IT ENDED ([`run_recorded_update_pass`]) — what the
/// schedulers read instead of `updated_at`. A clean pass is `ok`; a listing refused (the
/// cache stood in) stamps no success and is `failed`, exit 1 — it reached nothing; a pass
/// that reached nothing at all is `offline`; one whose index did not resolve is
/// `failed`. A vendor head-watch write after all of it changes none of it. There is no
/// rate-limit hold to record: the pass makes no metered request (owner ruling R3).
#[test]
fn a_full_pass_records_how_it_ended() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use aterm_update_core::pkg_check::PassOutcome;
    const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
    let layout = world::layout("pass-recorded");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let installed = crate::active_builds(&layout);
    let now = crate::flow::now_unix();
    let read = || crate::status::pass_stamps(&layout, now);
    let record = || crate::status::read(&layout).unwrap();
    // The next pass may end in the second the last success was stamped: age that stamp,
    // so an outcome after it is read by the outcome alone.
    let age_success = || {
        let success = record().last_success_at;
        std::fs::write(
            layout.status(),
            std::fs::read_to_string(layout.status()).unwrap().replace(
                &format!("last_success_at = \"{success}\""),
                "last_success_at = \"2026-09-01T00:00:00Z\"",
            ),
        )
        .unwrap();
    };
    // A clean pass: ok.
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    assert_eq!(read().last_outcome, Some(PassOutcome::Ok));
    // Refused: the listing's host answered 403, the cache stood in — nothing reached.
    *f.index.borrow_mut() = None;
    f.index_refused.set(true);
    age_success();
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    let refused = read();
    assert_eq!(refused.last_outcome, Some(PassOutcome::Failed));
    assert!(refused.last_failed());
    // Nothing answers: offline.
    f.index_refused.set(false);
    f.offline.set(true);
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        OFFLINE
    );
    assert_eq!(read().last_outcome, Some(PassOutcome::Offline));
    assert!(read().last_failed());
    // The index answers again: ok.
    f.offline.set(false);
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        0
    );
    let clean = read();
    assert_eq!(clean.last_outcome, Some(PassOutcome::Ok));
    assert!(!clean.last_failed());
    // A stale index the listing served: the pass did not resolve it — failed.
    *f.index.borrow_mut() = Some(world::signed_index(44, "2026-01-01T00:00:00Z", &[]));
    age_success();
    assert_eq!(
        run_recorded_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    let failed = read();
    assert_eq!(failed.last_outcome, Some(PassOutcome::Failed));
    assert!(failed.last_failed());
    // A later write of another kind (a row, as the vendor head watch writes it) moves
    // `updated_at` and nothing the schedulers read.
    write_program_row(&layout, "claude", record().programs["claude"].clone(), None);
    assert_eq!(read(), failed);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// TIER-1 OF THE DERIVED MODEL `AtpkgFullPassRule`, the queue: over every state either
/// arm of the model reaches with a child that waited behind a pass that has since ended
/// (`TakeLockBehind` enabled), the record laid down by that arm's writer, the real
/// stand-down ([`pass_ended_since_queued`], the child queued one full-pass count
/// before the pass that ended) stands down exactly when the fixed model's child does —
/// always — and the rule this replaced, standing down behind a success alone (kept
/// below as the negative control), exactly when `Buggy=1`'s does: it ran behind the
/// failed and the rate-limited pass the replayed queue ends on.
#[test]
fn a_queued_pass_stands_down_as_its_derived_model_does() {
    use crate::status::full_pass_rule_tier1::{AGE_SECS, reachable, realize, walk};
    use aterm_spec::derive::{Model, atpkg_full_pass_rule_model};
    let fixed = atpkg_full_pass_rule_model();
    let buggy = aterm_spec::interp::with_buggy(&fixed, 1);
    // ac5b4c144's `pass_finished_since_start`: a success stamped since the wait began.
    let legacy = |layout: &crate::store::Layout, first: i64| {
        crate::status::read(layout)
            .and_then(|s| aterm_update_core::pkg_check::rfc3339_to_unix(&s.last_success_at))
            .is_some_and(|success| success > first)
    };
    let child_runs = |model: &Model, state: &crate::status::full_pass_rule_tier1::State| {
        let mut next = state.clone();
        assert!(model.fire("TakeLockBehind", &mut next));
        next["running"] == 1
    };
    let mut seen = [0usize; 2];
    for (arm, model) in [(0, &fixed), (1, &buggy)] {
        let behind: Vec<_> = reachable(model)
            .into_iter()
            .filter(|s| s["behind"] >= 1 && s["running"] == 0)
            .collect();
        for (i, state) in behind.iter().enumerate() {
            assert_eq!(
                state["since_end"], 0,
                "a child takes the lock in the tick it freed"
            );
            let now = crate::flow::now_unix();
            let (layout, _) = realize(&format!("queue-{arm}-{i}"), state, arm == 0, now);
            // First contended while the pass that just ended still held the lock.
            let first = now - AGE_SECS[0] - 30;
            let stands_down = if arm == 0 {
                let stamps = crate::status::pass_stamps(&layout, now);
                let seq = crate::status::pass_seq(&layout);
                pass_ended_since_queued(true, seq.checked_sub(1), seq, &stamps, now, None).is_some()
            } else {
                legacy(&layout, first)
            };
            assert_eq!(
                stands_down,
                !child_runs(model, state),
                "arm {arm}: {state:?}"
            );
            seen[arm] += 1;
            let _ = std::fs::remove_dir_all(&layout.prefix);
        }
    }
    assert!(seen[0] > 0 && seen[1] > 0, "{seen:?}");
    // The replayed queue: two lanes decide in the gap, the first child's pass fails,
    // the second takes the lock behind it.
    let queue = ["SessionLook", "WindowWalk", "TakeLockFresh", "PassFails"];
    let now = crate::flow::now_unix();
    let first = now - AGE_SECS[0] - 30;
    let (layout, _) = realize("queue-defect", &walk(&fixed, &queue), true, now);
    let stamps = crate::status::pass_stamps(&layout, now);
    let seq = crate::status::pass_seq(&layout);
    assert_eq!(
        pass_ended_since_queued(true, seq.checked_sub(1), seq, &stamps, now, None)
            .map(|(outcome, _)| outcome),
        Some(aterm_update_core::pkg_check::PassOutcome::Failed),
        "the fixed child stands down behind the failure"
    );
    assert!(
        !legacy(&layout, first),
        "the old child ran the same failing pass again behind it"
    );
    assert!(child_runs(&buggy, &walk(&buggy, &queue)));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A STALE CACHE IS THE NETWORK'S ONLY WHEN NOTHING ANSWERED: the §14 cache aged past its
/// window while the machine was off the network is offline (retried soon, no failure
/// shown); the same stale index served by a host that answered — a publisher gone
/// quiet — or with the host refusing, is a failure.
#[test]
fn a_stale_cache_is_offline_only_when_nothing_answered() {
    use crate::vendor_direct::lane::world::{self, Fake};
    const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
    let layout = world::layout("pass-stale-offline");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2026-01-01T00:00:00Z", &[]));
    make_live(&layout, "ay", 18);
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "reached, and stale: the publisher's"
    );
    *f.index.borrow_mut() = None;
    f.offline.set(true);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        OFFLINE,
        "nothing answered: the cache aged out off the network"
    );
    f.index_refused.set(true);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "the index host answered with a refusal"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// AN UNREACHED PASS IS OFFLINE ONLY WHEN EVERY FAILURE WAS THE NETWORK'S. Nothing
/// answers and the cached index pins a build this machine lacks: the member lost at its
/// manifest fetch is the network's, and the pass is offline. The same pass whose member
/// failed at a step that needs no network — a row this client cannot stage, as a full
/// disk or a refused stage would — is a failure, whatever the network did.
#[test]
fn an_unreached_pass_is_offline_only_when_every_failure_was_a_fetch() {
    use crate::vendor_direct::lane::world::{self, Fake};
    const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
    let layout = world::layout("pass-offline-faults");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    make_live(&layout, "ay", 17);
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let installed = crate::active_builds(&layout);
    // Online, the index pins ay 18, whose manifest is not served: a failure (and the
    // index is cached now).
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    *f.index.borrow_mut() = None;
    f.offline.set(true);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        OFFLINE,
        "lost at its fetch while nothing answered"
    );
    f.publish_pkg_kind("ay", 18, current_triple(), "r", "system-package");
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "a failure no network would have fixed"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// ONE `status.toml` WRITE PER PASS (Phase 3). Unheld, an idle pass rewrote the whole
/// record once per writer — the outcome, each row, the success stamp, the freshness
/// stamp; held, as the CLI edge holds every verb that takes the store lock, the same
/// pass reaches the disk once, with the same record.
#[test]
fn an_idle_pass_writes_status_once_when_held() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let run_twice = |label: &str, held: bool| {
        let layout = world::layout(label);
        let f = Fake::default();
        *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
        f.publish_claude("2.1.281", &native_exe("a"));
        let cfg = crate::config::PackagesConfig::default();
        let trust = world::trust();
        let seams = world_seams(&trust);
        vendor_pass(
            &layout,
            &f,
            &cfg,
            &std::collections::BTreeMap::new(),
            VendorLanes {
                update_installed: true,
                complete_the_set: true,
            },
            Consent::Explicit,
            None,
            &seams,
        );
        let installed = crate::active_builds(&layout);
        // The first pass settles the rows; the second is the idle one measured.
        assert_eq!(
            run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
            0
        );
        let before = crate::status::counted::writes(&layout);
        {
            let _held = held.then(|| crate::status::hold(&layout));
            assert_eq!(
                run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
                0
            );
        }
        let writes = crate::status::counted::writes(&layout) - before;
        let record = crate::status::read(&layout).unwrap();
        let _ = std::fs::remove_dir_all(&layout.prefix);
        (writes, record)
    };
    let (unheld, _) = run_twice("idle-unheld", false);
    let (held, record) = run_twice("idle-held", true);
    assert!(unheld > 1, "the instrument sees the old cost: {unheld}");
    assert_eq!(held, 1, "held: one write for the whole pass");
    assert!(!record.last_success_at.is_empty());
    assert!(record.outcome.starts_with("up to date (index build 44)"));
}

/// The world's clock an hour on, and a day and an hour on.
fn world_now_plus_an_hour() -> i64 {
    crate::vendor_direct::lane::world::NOW + 60 * 60
}

fn world_now_plus_a_day() -> i64 {
    crate::vendor_direct::lane::world::NOW + 25 * 60 * 60
}

/// R1 FOR THE CLIENTS AT CUTOVER: the window's head-watch door moves an active legacy
/// index build at or below the legacy ceiling to the vendor's head with no index at
/// all — its version unread — rather than keeping it until ALab publishes again. One
/// above the ceiling (pinned after this code) the window's door keeps, with a row that
/// says why, and a person's door moves.
#[test]
fn a_legacy_build_moves_on_the_windows_door_with_no_index() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("legacy-window");
    make_live(&layout, "claude", 2_026_092_201);
    let f = Fake::default();
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    let trust = world::trust();
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let run = |layout: &crate::store::Layout, door| {
        update_vendor(layout, &f, claude, door, &world_seams(&trust))
    };
    assert!(run(&layout, VendorDoor::Window));
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.282").build_id())
    );
    // The door asked once, to refresh the yank latch; none answering blocked nothing.
    assert_eq!(f.index_reads.get(), 1, "only the latch refresh asked");
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let layout = world::layout("legacy-window-above");
    let above = claude.legacy_ceiling + 1;
    make_live(&layout, "claude", above);
    assert!(
        run(&layout, VendorDoor::Window),
        "kept on purpose: no fault"
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(above)
    );
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(
        row.state,
        "managed 2026092402 — its version could not be read; `aterm pkg update claude` \
         moves it to Anthropic's latest (2.1.282)"
    );
    assert_eq!(row.installed_build, Some(above));
    assert!(run(&layout, VendorDoor::Person));
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.282").build_id())
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// "LATEST" NEEDS A CHECK BEHIND IT: a vendor that cannot be reached leaves the row
/// alone for a day after the last check that reached it; past that the row says since
/// when it was not reached, and the managed-current marker stops naming the program.
#[test]
fn a_vendor_not_reached_for_a_day_is_no_longer_called_latest() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("unreached-day");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    let trust = world::trust();
    let claude = crate::vendor_direct::spec("claude").unwrap();
    let run = |now: fn() -> i64| {
        let seams = PassSeams {
            now,
            ..world_seams(&trust)
        };
        update_vendor(&layout, &f, claude, VendorDoor::Window, &seams)
    };
    let row = || {
        crate::status::read(&layout).unwrap().programs["claude"]
            .state
            .clone()
    };
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &world_seams(&trust)
    ));
    assert_eq!(row(), "managed 2.1.281 — Anthropic latest");
    f.offline.set(true);
    assert!(run(world_now_plus_an_hour));
    assert_eq!(
        row(),
        "managed 2.1.281 — Anthropic latest",
        "checked an hour ago"
    );
    assert!(run(world_now_plus_a_day));
    assert_eq!(
        row(),
        "managed 2.1.281 — Anthropic's release channel not reached since 2026-09-22"
    );
    let none = std::collections::BTreeSet::new();
    assert_eq!(
        managed_current_entries(&layout, &none, world_now_plus_a_day()),
        Vec::new()
    );
    // Reached again: latest again.
    f.offline.set(false);
    assert!(run(world_now_plus_a_day));
    assert_eq!(row(), "managed 2.1.281 — Anthropic latest");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE VENDOR DOORS SWEEP ONLY THEIR OWN PROGRAMS: the window's head-watch door, a
/// pass whose index lane cannot run, and `install <vendor>` each land a new claude,
/// and none of them sweeps trust's `.part` resume state or carried archive, or
/// reclaims another program's superseded build.
#[test]
fn a_vendor_door_sweeps_only_the_programs_its_lane_ran() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("door-gc");
    for b in [16, 17, 18] {
        make_live(&layout, "ay", b);
    }
    let staging = layout.staging_dir("trust");
    std::fs::create_dir_all(&staging).unwrap();
    let resume = staging.join("trust-9192.tar.zst.part");
    let carried = staging.join("trust-9191.tar.zst");
    for p in [&resume, &carried] {
        std::fs::write(p, b"half of a toolchain").unwrap();
    }
    let untouched = || {
        assert!(resume.exists(), "trust's resume state survives");
        assert!(carried.exists(), "trust's carried archive survives");
        assert!(
            layout.build_dir("ay", 16).exists(),
            "ay's builds are not ours"
        );
    };
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2026-01-01T00:00:00Z", &[]));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    f.publish_claude("2.1.281", &native_exe("a"));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &seams
    ));
    f.publish_claude("2.1.282", &native_exe("b"));
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Window,
        &seams
    ));
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.282").build_id())
    );
    untouched();
    f.publish_claude("2.1.283", &native_exe("c"));
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1,
        "the stale index stops the pass"
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.283").build_id())
    );
    untouched();
    assert!(
        !world::build_dir(&layout, "claude", "2.1.281").exists(),
        "claude's own superseded build is reclaimed"
    );
    f.publish_claude("2.1.284", &native_exe("d"));
    do_install_with(&layout, &f, "stable", "claude", &seams).expect("installed");
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.284").build_id())
    );
    untouched();
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `install <vendor>` RECORDS ITS ROW THROUGH THE LANE'S OWN WRITER: an install over a
/// yanked build that rolls back to the retained one says so — never "latest" while the
/// vendor's latest is the yanked build.
#[test]
fn an_install_that_rolls_back_off_a_yank_never_says_latest() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("install-rollback");
    let f = Fake::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    for (version, tag) in [("2.1.281", "a"), ("2.1.282", "b")] {
        f.publish_claude(version, &native_exe(tag));
        assert!(update_vendor(
            &layout,
            &f,
            claude,
            VendorDoor::Person,
            &seams
        ));
    }
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    crate::resolve_verified_index(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        vendor_floor(),
        world::NOW,
    )
    .expect("the yank is latched");
    let report = do_install_with(&layout, &f, "stable", "claude", &seams).expect("rolled back");
    assert_eq!(report.build, vendor_version("2.1.281").build_id());
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(
        row.state,
        "managed 2.1.281 — rolled back from yanked 2.1.282"
    );
    assert_eq!(row.installed_build, Some(report.build));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A PASS FAILED ONLY BY ITS VENDOR LANE IS NEVER HEADLINED "up to date": the index
/// resolves and applies clean, and the pass's outcome — the line Settings ▸ Packages
/// shows — names the vendor refusal, on the pass that met it and on every later one.
#[test]
fn a_pass_failed_only_by_its_vendor_lane_names_that_failure() {
    use crate::vendor_direct::lane::world::{self, CLAUDE, Fake, native_exe};
    let layout = world::layout("pass-outcome");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &seams
    ));
    f.publish_claude("2.1.282", &native_exe("b"));
    let forged = crate::openpgp::testkit::sign_detached(b"another document", 1_790_000_000);
    f.doc(&format!("{CLAUDE}2.1.282/manifest.json.sig"), &forged);
    let installed = crate::active_builds(&layout);
    for pass in ["first", "second"] {
        assert_eq!(
            run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
            1,
            "{pass}"
        );
        let outcome = crate::status::read(&layout).unwrap().outcome;
        assert!(
            outcome.starts_with("claude 2.1.282 refused: "),
            "{pass}: {outcome}"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A VENDOR FRESH-INSTALL REFUSAL STAYS ON ITS ROW through the set completion of the
/// same pass: the agent-rows loop never paints it `agent program — installing`.
#[test]
fn a_vendor_fresh_install_refusal_survives_set_completion() {
    use crate::vendor_direct::lane::world::{self, CLAUDE, Fake, native_exe};
    let layout = world::layout("refusal-row");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.282", &native_exe("a"));
    let forged = crate::openpgp::testkit::sign_detached(b"another document", 1_790_000_000);
    f.doc(&format!("{CLAUDE}2.1.282/manifest.json.sig"), &forged);
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ay".into(), "codex".into()]),
        ..Default::default()
    };
    let trust = world::trust();
    let seams = world_seams(&trust);
    for pass in ["first", "second"] {
        run_update_pass(
            &layout,
            &cfg,
            &f,
            &std::collections::BTreeMap::new(),
            true,
            &seams,
        );
        let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
        assert!(
            row.state.starts_with("error: claude 2.1.282 refused: "),
            "{pass}: {}",
            row.state
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A YANK BINDS THE PASS THAT FIRST SEES IT: the update pass and the toolset verb
/// verify the index before their vendor lane runs, so a yank published since the
/// last pass keeps the yanked head out now, not a pass or two later — and when no
/// index verifies, the vendor lane still runs, on the latch.
#[test]
fn a_yank_published_since_the_last_pass_binds_this_pass() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("yank-now");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let claude = crate::vendor_direct::spec("claude").unwrap();
    assert!(update_vendor(
        &layout,
        &f,
        claude,
        VendorDoor::Person,
        &seams
    ));
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    f.publish_claude("2.1.282", &native_exe("b"));
    let installed = crate::active_builds(&layout);
    run_update_pass(&layout, &cfg, &f, &installed, false, &seams);
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.281").build_id()),
        "the yanked head is kept out by the pass that first verified the yank"
    );
    // No index verifies (a stale one, which lifts no yank): the vendor lane still runs,
    // and the latch still binds.
    *f.index.borrow_mut() = Some(world::signed_index(46, "2026-01-01T00:00:00Z", &[]));
    f.publish_claude("2.1.282", &native_exe("b"));
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.281").build_id()),
        "the latched yank binds"
    );
    f.publish_claude("2.1.283", &native_exe("c"));
    assert_eq!(
        run_update_pass(&layout, &cfg, &f, &installed, false, &seams),
        1
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.283").build_id()),
        "never blocked by the index"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    // The toolset verb, on a machine with nothing installed and no latch yet.
    let layout = world::layout("yank-now-toolset");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(
        45,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.282"],
    ));
    f.publish_claude("2.1.282", &native_exe("b"));
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ay".into(), "codex".into()]),
        ..Default::default()
    };
    install_toolset(&layout, &cfg, &f, &seams);
    assert!(
        !crate::active_builds(&layout).contains_key("claude"),
        "the yanked head is never a fresh install"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A TOMBSTONED VENDOR PROGRAM IS REVISITED by an update pass on a machine that never
/// adopted the set, as the index lane revisits one: the shim view is silent for it,
/// and once the vendor ships an admissible build the pass installs it — even with
/// nothing else installed.
#[test]
fn a_tombstoned_vendor_program_comes_back_with_an_admissible_head() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let layout = world::layout("pass-revive");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("a"));
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let seams = world_seams(&trust);
    let lanes = VendorLanes {
        update_installed: true,
        complete_the_set: false,
    };
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    // A fresh index yanks 2.1.281 while the head still names it and nothing is
    // retained: the lane tombstones it.
    *f.index.borrow_mut() = Some(world::signed_index(
        44,
        "2099-01-01T00:00:00Z",
        &["claude@2.1.281"],
    ));
    crate::resolve_verified_index(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        vendor_floor(),
        world::NOW,
    )
    .expect("fresh");
    let pass = vendor_pass(
        &layout,
        &f,
        &cfg,
        &crate::active_builds(&layout),
        lanes,
        Consent::Explicit,
        None,
        &seams,
    );
    assert_eq!(pass.failures, 1, "the tombstone is a fault");
    assert!(
        !crate::active_builds(&layout).contains_key("claude"),
        "PRECONDITION: tombstoned, and silent in the shim view"
    );
    assert!(vendor_tombstoned_in_place(&layout));
    f.publish_claude("2.1.282", &native_exe("b"));
    assert_eq!(
        run_update_pass(
            &layout,
            &cfg,
            &f,
            &std::collections::BTreeMap::new(),
            false,
            &seams
        ),
        0
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.282").build_id())
    );
    assert!(!vendor_tombstoned_in_place(&layout));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The vendor lane's fresh installs are a progress pass of their own: their bytes
/// meter on the window's progress file before the index lane's pass begins.
#[test]
fn a_vendor_fresh_install_streams_on_the_progress_file() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    let _gate = crate::progress::PASS_TEST_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let layout = world::layout("pass-progress");
    let f = Fake::default();
    let payload = native_exe("claude 2.1.281");
    f.publish_claude("2.1.281", &payload);
    let cfg = crate::config::PackagesConfig::default();
    let trust = world::trust();
    let progress = layout.progress_file();
    let seams = PassSeams {
        progress: Some(&progress),
        ..world_seams(&trust)
    };
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: false,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    let file = crate::progress::read_progress(&layout).expect("a progress file");
    assert_eq!(file.pass, "net");
    assert!(file.ended_unix.is_some(), "the pass ended");
    assert_eq!(file.overall.bytes_total, payload.len() as u64);
    assert_eq!(file.overall.programs_done, 1);
    assert_eq!(file.programs["claude"].phase, crate::progress::Phase::Done);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A FIRST LANDING WHOSE SIGNATURE CHECK DID NOT FINISH IS NO FAILURE (review of
/// 4329ea590, 2026-09-26). That commit made a codesign timeout `Verdict::Deferred` and
/// quiet on the update lane, for an INSTALLED program; the set-completion lane's fresh
/// path (`update` with `complete_the_set`, `install --default-set`) still put every
/// non-Installed verdict on a Failed progress row and, with nothing arrived, printed
/// `net-failed:` — the window's `PkgSeedFailed`, "Package update failed" — on exactly
/// the pass most likely to time out: codex's 238 MB binary laid for the first time.
/// Now: a Skipped row with no error, no failure counted, no `net-failed:`, the
/// positive `seed-done:` terminal naming what waits, and the next pass lands the kept
/// archive without a download. Control: a real signer refusal on the same fresh path
/// still fails its row and still says `net-failed:`.
#[test]
fn a_fresh_landing_whose_signature_check_did_not_finish_is_no_failure() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    // The Apple anchor exists on macOS only: off it no codesign check runs at all.
    if !cfg!(target_os = "macos") {
        return;
    }
    let _gate = crate::progress::PASS_TEST_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let cfg = crate::config::PackagesConfig::default();
    let lanes = VendorLanes {
        update_installed: true,
        complete_the_set: true,
    };
    let pass_over =
        |layout: &crate::store::Layout, f: &Fake, trust: &crate::vendor_direct::lane::Trust| {
            let progress = layout.progress_file();
            let seams = PassSeams {
                progress: Some(&progress),
                ..world_seams(trust)
            };
            let _ = take_markers_said();
            let pass = vendor_pass(
                layout,
                f,
                &cfg,
                &crate::active_builds(layout),
                lanes,
                Consent::Explicit,
                None,
                &seams,
            );
            let file = crate::progress::read_progress(layout).expect("a progress file");
            (pass, take_markers_said(), file)
        };
    let is = |said: &[String], marker: &str| {
        said.iter()
            .any(|l| l.starts_with(&format!("atpkg: {marker}")))
    };
    let layout = world::layout("fresh-deferred");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    let (pass, said, file) = pass_over(&layout, &f, &world::trust_timing_out());
    assert_eq!(pass.failures, 0, "a wait is no failure: {said:?}");
    assert!(pass.failed.is_empty());
    assert!(
        !is(&said, NET_FAILED_MARKER),
        "no \"Package update failed\" for a check that did not finish: {said:?}"
    );
    assert!(
        said.iter().any(|l| l
            == &format!(
                "atpkg: {SEED_DONE_MARKER}the pass finished; claude waits for its \
                 signature check to finish — the next pass installs it"
            )),
        "the announcement is answered, and says what waits: {said:?}"
    );
    let row = &file.programs["claude"];
    assert_eq!(row.phase, crate::progress::Phase::Skipped, "{row:?}");
    assert_eq!(row.error, None);
    assert!(!crate::active_builds(&layout).contains_key("claude"));
    // The next pass, whose check finishes, lands the kept archive: no download.
    let downloads = f.downloads.borrow().len();
    let (pass, said, file) = pass_over(&layout, &f, &world::trust());
    assert_eq!(pass.failures, 0);
    assert!(is(&said, NET_INSTALLED_MARKER), "{said:?}");
    assert_eq!(file.programs["claude"].phase, crate::progress::Phase::Done);
    assert_eq!(
        crate::active_builds(&layout).get("claude").copied(),
        Some(vendor_version("2.1.281").build_id())
    );
    assert_eq!(
        f.downloads.borrow().len(),
        downloads,
        "the verified archive was kept for the retry"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    // CONTROL: a real refusal on the same fresh path is still a failure, said.
    let layout = world::layout("fresh-refused");
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    let (pass, said, file) = pass_over(&layout, &f, &world::trust_refusing_signers());
    assert_eq!(pass.failures, 1, "{said:?}");
    assert!(is(&said, NET_FAILED_MARKER), "{said:?}");
    assert!(!is(&said, SEED_DONE_MARKER), "{said:?}");
    assert_eq!(
        file.programs["claude"].phase,
        crate::progress::Phase::Failed
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The fresh-landing terminal, case by case: something arrived → `net-installed:`
/// (a deferral beside it said on its own line); nothing arrived and one landing
/// failed → `net-failed:`, even beside a deferral; every landing deferred →
/// `seed-done:`, with the words a person reads whole; nothing planned → nothing.
#[test]
fn the_fresh_landing_terminal_fails_only_on_a_failure() {
    assert_eq!(fresh_landing_terminal(&[], &[], 0), None);
    assert_eq!(
        fresh_landing_terminal(&["claude"], &["codex"], 2).as_deref(),
        Some("atpkg: net-installed: claude")
    );
    assert!(
        fresh_landing_terminal(&[], &["codex"], 2)
            .is_some_and(|l| l.starts_with(&format!("atpkg: {NET_FAILED_MARKER}"))),
        "one deferred, one failed: the failure is said"
    );
    let both = fresh_landing_terminal(&[], &["claude", "codex"], 2).unwrap();
    assert_eq!(
        both,
        format!(
            "atpkg: {SEED_DONE_MARKER}the pass finished; claude, codex wait for their \
             signature check to finish — the next pass installs them"
        )
    );
    assert_eq!(
        human_marker(&both).as_deref(),
        Some(
            "The pass finished; claude, codex wait for their signature check to finish — \
             the next pass installs them."
        ),
        "a person reads what waits, not a bare \"Done.\""
    );
}

/// THE DEFAULT SET NAMES A MISSING C TOOLCHAIN, over the `PATH` it ran with: a stubbed
/// PATH with no `cc` adds `doctor`'s own sentence and its one act; one whose `cc`
/// answers adds nothing; one whose `cc` refuses names what it said. (Before, the set
/// said "complete" over a machine whose first `targo build` died in a build script.)
/// The probe's bound is widened the way `prereq`'s answering tests widen it: the
/// first exec of a script this test just wrote waits on the system's code
/// assessment, which a loaded machine stretches past the production 5 s — and an
/// unanswered probe names nothing, so the refusal case would fail and the answering
/// one pass vacuously.
#[cfg(unix)]
#[test]
fn the_default_set_tail_names_a_missing_c_toolchain() {
    use std::os::unix::fs::PermissionsExt as _;
    let host = crate::prereq::CcHost {
        bound: std::time::Duration::from_secs(120),
        ..crate::prereq::CcHost::this_machine()
    };
    let root = crate::testdir::dir("atpkg-default-set-cc-");
    let bare = root.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let line = default_set_prereq_line(Some(bare.as_os_str()), &host).expect("no cc is named");
    assert_eq!(
        line,
        format!("atpkg: {}", crate::prereq::CcVerdict::NoDriver.line())
    );
    assert!(
        line.ends_with(&format!("fix: {}", crate::prereq::act())),
        "{line}"
    );
    let stub = |dir: &str, body: &str| {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cc"), format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(dir.join("cc"), std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    };
    // A `cc` that answers AND builds the probe program (`-o <out> <src>`).
    let good = stub(
        "good",
        "case \"$1\" in -o) printf x > \"$2\";; *) echo cc 1;; esac",
    );
    assert_eq!(default_set_prereq_line(Some(good.as_os_str()), &host), None);
    let bad = stub("bad", "echo 'cc: cannot find crt1.o' >&2; exit 1");
    let line = default_set_prereq_line(Some(bad.as_os_str()), &host).expect("a refusal is named");
    assert!(line.contains("(cc: cannot find crt1.o)"), "{line}");
    let _ = std::fs::remove_dir_all(&root);
}

/// NOT COMPLETE WHILE A VENDOR PROGRAM IS STILL TO COME: the explicit toolset verb
/// over a vendor that did not serve its head exits 1 naming it — never "already
/// complete" — and still adopts the set its index lane completed.
#[test]
fn the_toolset_verb_is_incomplete_while_a_vendor_program_is_missing() {
    use crate::vendor_direct::lane::world::{self, CLAUDE, Fake};
    let layout = world::layout("toolset-vendor");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_codex_default("0.157.0");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ay".into()]),
        ..Default::default()
    };
    let trust = world::trust();
    let seams = world_seams(&trust);
    vendor_pass(
        &layout,
        &f,
        &cfg,
        &std::collections::BTreeMap::new(),
        VendorLanes {
            update_installed: false,
            complete_the_set: true,
        },
        Consent::Explicit,
        None,
        &seams,
    );
    assert!(crate::active_builds(&layout).contains_key("codex"));
    // Anthropic's host does not serve the head.
    f.forget(&format!("{CLAUDE}latest"));
    assert_eq!(install_toolset(&layout, &cfg, &f, &seams), 1);
    assert!(
        adopted(&layout),
        "the index lane's set is complete: adopted"
    );
    assert!(!crate::active_builds(&layout).contains_key("claude"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// NO INDEX RULE DECIDES A VENDOR PROGRAM through the set lane or the explicit door:
/// an index `system = "claude"` with a claude on PATH retires nothing, and `install
/// claude` asks the index nothing before its own lane.
#[cfg(unix)]
#[test]
fn an_index_system_key_never_retires_a_vendor_program() {
    use std::os::unix::fs::PermissionsExt as _;
    let layout = temp_layout("vendor-system");
    let index = index_of(&[("ay", None), ("claude", Some("claude"))]);
    let sys = layout
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-vendor-system-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sys);
    std::fs::create_dir_all(&sys).unwrap();
    let exe = sys.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([sys.clone()]).unwrap();
    let mut installed = std::collections::BTreeMap::from([(
        "claude".to_string(),
        vendor_version("2.1.281").build_id(),
    )]);
    assert!(
        reconcile_system_satisfied_with(
            &layout,
            &index,
            &names(&["claude"]),
            &mut installed,
            Some(&path)
        )
        .is_empty()
    );
    assert!(installed.contains_key("claude"), "nothing retired");
    let f = crate::vendor_direct::lane::world::Fake::default();
    assert_eq!(explicit_install_door(&layout, &f, "claude"), None);
    assert_eq!(
        f.index_reads.get(),
        0,
        "no index resolve ahead of the vendor lane"
    );
    let _ = std::fs::remove_dir_all(&sys);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `list` names a vendor build by its version, never its 19-digit store id; an index
/// build keeps its number, byte for byte.
#[test]
fn list_names_a_vendor_build_by_version() {
    let claude = vendor_version("2.1.280").build_id();
    let installed = vec![("claude".to_string(), claude), ("trust".to_string(), 8595)];
    let live = std::collections::BTreeMap::from([
        ("claude".to_string(), claude),
        ("trust".to_string(), 8595),
    ]);
    let linked = std::collections::BTreeMap::new();
    let pinned = std::collections::BTreeSet::new();
    assert_eq!(
        list_porcelain_lines(&installed, &live, &linked, &pinned),
        ["claude\t2.1.280\tlive", "trust\t8595\tlive"]
    );
    let human = list_human_lines(&installed, &[], &live, &linked, &pinned);
    assert!(
        human
            .iter()
            .any(|l| l.contains("claude") && l.contains("2.1.280")),
        "{human:?}"
    );
    assert!(
        human.iter().all(|l| !l.contains(&claude.to_string())),
        "{human:?}"
    );
}

/// `atpkg rollback <vendor program>` (design §1.3) never reaches the index — the verb
/// builds no fetcher for it at all: it moves to the newest retained vendor build no
/// signed yank names, shims the table's exposes, records a row that says why the head
/// is not what runs (with the build's own root, so `atpkg verify` passes) and says so
/// by version; with nothing retained, or only a yanked build, it refuses and nothing
/// moves.
#[test]
fn a_vendor_rollback_moves_to_the_retained_build_without_the_index() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("rollback-by-hand");
    let f = Fake::default();
    let trust = world::trust();
    let none = Policy::default();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let install = || {
        let lane = lane::Lane {
            layout: &layout,
            fetcher: &f,
            triple: world::triple(),
            policy: &none,
            trust: &trust,
            legacy_index: None,
            asked: false,
            now: world::NOW,
        };
        lane::install_one(&lane, spec, false)
    };
    let no_index =
        || -> Box<dyn crate::flow::Fetcher> { panic!("a vendor rollback reached the index lane") };
    let rollback = || rollback_in(&layout, "stable", "claude", &[], &no_index);
    let active = || crate::active_builds(&layout).get("claude").copied();
    let err = lane::roll_back_by_hand(&layout, &none, spec).unwrap_err();
    assert!(err.contains("not installed"), "{err}");
    assert_eq!(rollback(), ExitCode::from(1));
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    assert!(matches!(install().verdict, Verdict::Installed { .. }));
    let err = lane::roll_back_by_hand(&layout, &none, spec).unwrap_err();
    assert_eq!(err, "no older claude build is kept — claude 2.1.281 stays");
    f.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    assert!(matches!(install().verdict, Verdict::Installed { .. }));
    let (v281, v282) = (vendor_version("2.1.281"), vendor_version("2.1.282"));
    // The retained build is yanked by the latch the verb reads: refused by version,
    // and nothing moves.
    let yanked = Policy::from_entries(["claude@2.1.281"]);
    let err = lane::roll_back_by_hand(&layout, &yanked, spec).unwrap_err();
    assert_eq!(
        err,
        "claude 2.1.281 is yanked and no older build is kept — claude 2.1.282 stays"
    );
    let latch = crate::vendor_direct::policy::latch_path(&layout);
    std::fs::write(
        &latch,
        "schema = 1\nindex_build = 44\n[yanks]\nclaude = [\"2.1.281\"]\n",
    )
    .unwrap();
    assert_eq!(rollback(), ExitCode::from(1));
    assert_eq!(active(), Some(v282.build_id()));
    std::fs::remove_file(&latch).unwrap();
    // Admissible: it moves, through the verb's body, with no index read.
    let reads = f.index_reads.get();
    assert_eq!(rollback(), ExitCode::SUCCESS);
    assert_eq!(active(), Some(v281.build_id()));
    assert_eq!(f.index_reads.get(), reads, "the index is never consulted");
    assert!(
        crate::which(&layout, "claude")
            .unwrap()
            .starts_with(layout.build_dir("claude", v281.build_id())),
        "the shim runs the retained build"
    );
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(row.installed_build, Some(v281.build_id()));
    assert_eq!(row.state, "managed 2.1.281 — rolled back from 2.1.282");
    let record =
        crate::vendor_direct::complete_record(&layout.build_dir("claude", v281.build_id()))
            .unwrap();
    assert!(!row.tree_root.is_empty());
    assert_eq!(row.tree_root, record.tree_root);
    assert!(
        matches!(
            crate::verify::verify_program(&layout, "claude"),
            crate::verify::VerifyOutcome::VendorRoot { drift: None, .. }
        ),
        "atpkg verify passes on the rolled-back build"
    );
    let line = vendor_rolled_back_line(spec, "2.1.282", "2.1.281", false);
    assert_eq!(
        line,
        "atpkg: rolled back claude from 2.1.282 to 2.1.281; `aterm pkg pin claude` holds it \
         there (otherwise the next update returns it to Anthropic's latest)"
    );
    assert_eq!(crate::vendor_direct::retired_wording(&line), None);
    // Already pinned, the vendor lane leaves it where it lands: the index lane's words.
    let pinned = vendor_rolled_back_line(spec, "2.1.282", "2.1.281", true);
    assert_eq!(
        pinned,
        "atpkg: rolled back claude from 2.1.282 to 2.1.281; it is pinned, so updates leave \
         it there until `aterm pkg unpin claude`"
    );
    assert_eq!(crate::vendor_direct::retired_wording(&pinned), None);
    // gc keeps below the live build the one this verb lands on: .283 live, .282 and .281
    // retained (a session still running .281 kept it through a pass), and the latch
    // yanking .282. `atpkg gc`'s own call reclaims the yanked .282 and keeps .281, so
    // the verb still has a build to land on.
    f.publish_claude("2.1.283", &native_exe("claude 2.1.283"));
    assert!(matches!(install().verdict, Verdict::Installed { .. }));
    std::fs::write(
        &latch,
        "schema = 1\nindex_build = 44\n[yanks]\nclaude = [\"2.1.282\"]\n",
    )
    .unwrap();
    let report = crate::gc::run(&layout, last_verified_channel(&layout).as_ref());
    assert_eq!(
        report.reclaimed,
        vec![("claude".to_string(), vec![v282.build_id()])]
    );
    assert_eq!(rollback(), ExitCode::SUCCESS);
    assert_eq!(active(), Some(v281.build_id()));
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A legacy build's row carries a root only when the recorded row was captured for that
/// same build: the shims run legacy build B while the record names legacy build A (an
/// older client's pass on a shared store), and the vendor lane keeps B (a local hold) —
/// the row names B with no root, never B attested by A's, which would disarm verify's
/// BuildMismatch guard.
#[cfg(unix)]
#[test]
fn a_kept_legacy_row_never_carries_another_builds_root() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("legacy-root");
    let (a, b) = (2_026_091_801_u64, 2_026_091_901_u64);
    for build in [a, b] {
        let dir = layout.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/claude"), native_exe("legacy")).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
    }
    crate::activate::install_shims(
        &layout,
        &layout.build_dir("claude", b),
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    let root_a = "a".repeat(64);
    let recorded = |build: u64| crate::ProgramStatus {
        installed_build: Some(build),
        state: crate::state::managed(build, 40),
        tree_root: root_a.clone(),
    };
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("vendor"));
    let trust = world::trust();
    let none = Policy::default();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let keep = || {
        let lane = lane::Lane {
            layout: &layout,
            fetcher: &f,
            triple: world::triple(),
            policy: &none,
            trust: &trust,
            legacy_index: None,
            asked: false,
            now: world::NOW,
        };
        let outcome = lane::install_one(&lane, spec, true);
        assert!(
            matches!(outcome.verdict, Verdict::Kept { .. }),
            "{outcome:?}"
        );
        record_vendor_status(&layout, &outcome, world::NOW);
        crate::status::read(&layout).unwrap().programs["claude"].clone()
    };
    record_status(&layout, "claude", recorded(a), "x".into());
    let row = keep();
    assert_eq!(row.installed_build, Some(b));
    assert_eq!(row.tree_root, "", "A's root never attests B");
    // The record for B itself keeps its root.
    record_status(&layout, "claude", recorded(b), "x".into());
    let row = keep();
    assert_eq!(row.installed_build, Some(b));
    assert_eq!(row.tree_root, root_a);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// Right after the cutover the only retained build below the first vendor build is the
/// legacy index build it replaced — the one `list` shows as kept for rollback — and
/// `rollback` moves to it: shimmed with the table's env (not the index's), its row
/// naming it by number with no index pin, and a signed yank on its number refusing it.
#[cfg(unix)]
#[test]
fn a_vendor_rollback_returns_to_the_legacy_build_the_first_vendor_install_replaced() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("rollback-legacy");
    let legacy = layout.build_dir("claude", 2_026_091_901);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join("bin/claude"), native_exe("legacy")).unwrap();
    // The index manifest's env, which the table overrides.
    crate::shim_env::write_sidecar(
        &legacy,
        &crate::shim_env::ShimEnv::admit(&["FROM_INDEX=1".to_string()]).unwrap(),
    )
    .unwrap();
    crate::activate::install_shims(
        &layout,
        &legacy,
        &["claude".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("vendor"));
    let trust = world::trust();
    let none = Policy::default();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let lane = lane::Lane {
        layout: &layout,
        fetcher: &f,
        triple: world::triple(),
        policy: &none,
        trust: &trust,
        legacy_index: None,
        asked: false,
        now: world::NOW,
    };
    assert!(matches!(
        lane::install_one(&lane, spec, false).verdict,
        Verdict::Installed { .. }
    ));
    let no_index =
        || -> Box<dyn crate::flow::Fetcher> { panic!("a vendor rollback reached the index lane") };
    // A signed yank on the legacy number refuses it, and nothing moves.
    let yanked = Policy::from_entries(["claude@2026091901"]);
    let err = lane::roll_back_by_hand(&layout, &yanked, spec).unwrap_err();
    assert_eq!(
        err,
        "claude build 2026091901 is yanked and no older build is kept — claude 2.1.281 stays"
    );
    let v281 = vendor_version("2.1.281").build_id();
    assert_eq!(crate::active_builds(&layout).get("claude"), Some(&v281));
    assert_eq!(
        rollback_in(&layout, "stable", "claude", &[], &no_index),
        ExitCode::SUCCESS
    );
    assert_eq!(
        crate::active_builds(&layout).get("claude"),
        Some(&2_026_091_901)
    );
    let shim = layout.shim(&crate::store::ToolName::new("claude").unwrap());
    assert_eq!(
        shim_env_fix(&shim, "claude").as_deref(),
        Some(
            "Claude Code's own updater is off here (DISABLE_AUTOUPDATER=1); also runs with \
             CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1"
        ),
        "the table's env, never the index manifest's"
    );
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(row.installed_build, Some(2_026_091_901));
    assert_eq!(row.state, "managed 2026091901 — rolled back from 2.1.281");
    assert!(row.tree_root.is_empty(), "no root was ever recorded for it");
    let line = vendor_rolled_back_line(spec, "2.1.281", "build 2026091901", false);
    for text in [&row.state, &line] {
        assert_eq!(crate::vendor_direct::retired_wording(text), None, "{text}");
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A ROLLBACK TO THE LEGACY BUILD IS ATTESTED, with no index asked: the first vendor
/// install overwrites the row that carried the legacy build's signed root, so the root is
/// kept in the program's stamp — from that row as it is replaced, or from the signed
/// manifest the lane read the build's version from — and the rollback's row carries it,
/// so `atpkg verify` matches instead of saying "no signed tree_root". A rollback row an
/// earlier client recorded without it is attested all the same, and the next pass that
/// keeps the build writes the root into it. A root nothing holds (a first vendor install
/// by a client that kept none, or a stamp an older client rewrote) is read again from
/// the build's signed manifest by the next pass that holds a verified index.
#[cfg(unix)]
#[test]
fn a_rollback_to_the_legacy_build_is_attested_by_its_kept_root() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let legacy_build = 2_026_091_901_u64;
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let no_index =
        || -> Box<dyn crate::flow::Fetcher> { panic!("a vendor rollback reached the index lane") };
    let legacy_install = |layout: &crate::store::Layout| {
        let dir = layout.build_dir("claude", legacy_build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/claude"), native_exe("legacy")).unwrap();
        crate::activate::install_shims(
            layout,
            &dir,
            &["claude".to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        crate::tree::tree_root(&dir).unwrap()
    };
    let trust = world::trust();
    let none = Policy::default();
    let pass = |layout: &crate::store::Layout,
                f: &Fake,
                index: Option<&crate::TrustedIndex>,
                held: bool| {
        let lane = lane::Lane {
            layout,
            fetcher: f,
            triple: world::triple(),
            policy: &none,
            trust: &trust,
            legacy_index: index,
            asked: false,
            now: world::NOW,
        };
        let outcome = lane::install_one(&lane, spec, held);
        record_vendor_status(layout, &outcome, world::NOW);
        outcome
    };
    let row = |layout: &crate::store::Layout| {
        crate::status::read(layout).unwrap().programs["claude"].clone()
    };

    // The recorded one: the legacy build's row carried its signed root.
    let layout = world::layout("rollback-legacy-row");
    let root = legacy_install(&layout);
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(legacy_build),
            state: crate::state::managed(legacy_build, 44),
            tree_root: root.clone(),
        },
        "x".into(),
    );
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("vendor"));
    assert!(matches!(
        pass(&layout, &f, None, false).verdict,
        Verdict::Installed { .. }
    ));
    assert_ne!(
        row(&layout).tree_root,
        root,
        "the vendor build's row replaced it"
    );
    assert_eq!(
        rollback_in(&layout, "stable", "claude", &[], &no_index),
        ExitCode::SUCCESS
    );
    assert_eq!(row(&layout).installed_build, Some(legacy_build));
    assert_eq!(row(&layout).tree_root, root);
    assert_eq!(
        crate::verify::verify_program(&layout, "claude"),
        crate::verify::VerifyOutcome::Match {
            build: legacy_build
        }
    );
    // A rollback row recorded without the root (an earlier client's): verify still
    // attests the build, and the next pass that keeps it heals the row.
    let mut bare = row(&layout);
    bare.tree_root = String::new();
    record_status(&layout, "claude", bare, "x".into());
    assert_eq!(
        crate::verify::verify_program(&layout, "claude"),
        crate::verify::VerifyOutcome::Match {
            build: legacy_build
        }
    );
    assert!(matches!(
        pass(&layout, &f, None, true).verdict,
        Verdict::Kept { .. }
    ));
    assert_eq!(row(&layout).installed_build, Some(legacy_build));
    assert_eq!(
        row(&layout).tree_root,
        root,
        "the kept pass wrote the root back"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);

    // Its pkg manifest: no row ever carried the root, and the lane read the build's
    // version — and root — from its signed manifest before replacing it.
    let layout = world::layout("rollback-legacy-pkg");
    let root = legacy_install(&layout);
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg_rooted("claude", legacy_build, "2.1.281", world::triple(), &root);
    let index = crate::resolve_verified_index(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        world::NOW,
    )
    .expect("the index verifies");
    f.publish_claude("2.1.282", &native_exe("vendor"));
    assert!(matches!(
        pass(&layout, &f, Some(&index), false).verdict,
        Verdict::Installed { .. }
    ));
    *f.index.borrow_mut() = None;
    f.forget_legacy_pkgs();
    assert_eq!(
        rollback_in(&layout, "stable", "claude", &[], &no_index),
        ExitCode::SUCCESS
    );
    assert_eq!(row(&layout).tree_root, root);
    assert_eq!(
        crate::verify::verify_program(&layout, "claude"),
        crate::verify::VerifyOutcome::Match {
            build: legacy_build
        }
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);

    // Nothing holds it: the vendor install kept the replaced row's root, and an older
    // client then rewrote the stamp from its own fields (a client before 2026-09-23 never
    // kept it at all). The next pass holding a verified index reads it again.
    let layout = world::layout("rollback-legacy-unkept");
    let root = legacy_install(&layout);
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: Some(legacy_build),
            state: crate::state::managed(legacy_build, 44),
            tree_root: root.clone(),
        },
        "x".into(),
    );
    let f = Fake::default();
    f.publish_claude("2.1.282", &native_exe("vendor"));
    assert!(matches!(
        pass(&layout, &f, None, false).verdict,
        Verdict::Installed { .. }
    ));
    let stamp = crate::vendor_direct::stamp_path(&layout, "claude").unwrap();
    let older: String = std::fs::read_to_string(&stamp)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("legacy_root_build") && !l.starts_with("legacy_tree_root"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&stamp, older).unwrap();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_legacy_pkg_rooted("claude", legacy_build, "2.1.281", world::triple(), &root);
    let index = crate::resolve_verified_index(
        &f,
        &layout,
        &crate::sig::testkit::anchor(),
        crate::sig::BuildFloor {
            index_build: 0,
            roster_seq: crate::sig::testkit::SEQ,
        },
        world::NOW,
    )
    .expect("the index verifies");
    assert!(matches!(
        pass(&layout, &f, Some(&index), false).verdict,
        Verdict::Current(_)
    ));
    *f.index.borrow_mut() = None;
    f.forget_legacy_pkgs();
    assert_eq!(
        rollback_in(&layout, "stable", "claude", &[], &no_index),
        ExitCode::SUCCESS
    );
    assert_eq!(row(&layout).tree_root, root);
    assert_eq!(
        crate::verify::verify_program(&layout, "claude"),
        crate::verify::VerifyOutcome::Match {
            build: legacy_build
        }
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// An index program's "already current" names the index that pins the build, so it can
/// be set beside the channel head doctor reports (2026-09-22: the line said only the
/// build, and nothing on screen said which index that was current with).
#[test]
fn already_current_names_the_index_that_pins_it() {
    let r = crate::flow::InstallReport {
        program: "ay".into(),
        build: 8256,
        index_build: 42,
        roster_seq: 4,
        already_current: true,
        shimmed: vec![],
        refused_shims: vec![],
        tree_root: String::new(),
        dependencies: vec![],
        vendor: None,
    };
    assert_eq!(
        super::already_current_line(&r),
        "atpkg: ay already current (build 8256, as index 42 pins it)"
    );
}
