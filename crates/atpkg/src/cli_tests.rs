// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[path = "vendor_plan_effects_tests.rs"]
mod vendor_plan_effects;

#[test]
fn conditional_link_options_are_parsed_without_becoming_bin_paths() {
    let args: Vec<String> = [
        "codex",
        "/new",
        "bin/codex",
        "--if-checkout",
        "/old",
        "--if-prefix",
        "/pkg",
        "--wait-lock",
        "1800",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let request = super::link_args(&args).unwrap();
    assert_eq!(request.bins, [std::path::PathBuf::from("bin/codex")]);
    assert_eq!(request.if_checkout, Some(std::path::Path::new("/old")));
    assert_eq!(request.if_prefix, Some(std::path::Path::new("/pkg")));
    assert_eq!(
        request.wait_lock,
        Some(std::time::Duration::from_secs(1800))
    );
    for tail in [
        vec!["--if-checkout"],
        vec!["--if-checkout", "/old"],
        vec!["--if-prefix", "/pkg"],
        vec!["--if-prefix", "--if-checkout", "/old"],
        vec!["--if-checkout", "/old", "--if-checkout", "/other"],
        vec!["--unknown", "/old"],
        vec!["--wait-lock"],
        vec!["--wait-lock", "soon"],
        vec!["--wait-lock", "--if-prefix", "/pkg"],
        vec!["--wait-lock", "1", "--wait-lock", "2"],
    ] {
        let bad: Vec<String> = ["codex", "/new", "bin/codex"]
            .into_iter()
            .chain(tail)
            .map(str::to_owned)
            .collect();
        assert!(super::link_args(&bad).is_err());
    }
    let plain: Vec<String> = ["codex", "/new", "--wait-lock", "60"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let plain = super::link_args(&plain).unwrap();
    assert!(plain.if_checkout.is_none() && plain.if_prefix.is_none());
    assert_eq!(plain.wait_lock, Some(std::time::Duration::from_secs(60)));
}

/// Every fixture pass runs against an empty `PATH`: a pass consults `PATH` for a foreign
/// copy ahead of ours (`reconcile_shadowed`) and for a system install that satisfies a
/// program (`reconcile_system_satisfied_with`), so a fixture reading the developer's
/// `PATH` would judge a temp prefix against the real machine. Empty, not the real one
/// filtered: only that can never acquire a foreign binary later. A pass that exercises
/// shadowing calls [`super::install_default_set_with_path`] with its own `PATH`. The
/// consent shapes only the wire lane's announcement, which `net_announcement`'s tests pin.
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
        super::SetCompanions::alone(),
    )
}

/// The `--wait-lock <secs>` edge grammar: stripped for the three provisioning verbs
/// wherever it sits, the last parsable value wins, and an unparsable value or a bare flag
/// at the end of argv is consumed and means "no wait", never a program name. (A valueless
/// flag mid-argv eats the next token; no spawner produces that shape, see
/// `take_wait_lock_flag`.)
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

/// What a discovery may guess the channel publishes: a store that verified nothing takes
/// the highest build its cache holds, its release cut compiled in, or the window's probe
/// saw, and where the run starts from the cut; a store with a floor takes only the probe's
/// build, and never reads its cache for this.
#[test]
fn a_discovery_guesses_from_every_build_seen_published() {
    use crate::net::IndexHint;
    let hint = |low, newest| Some(IndexHint { low, newest });
    let cut = hint(Some(8), 72);
    assert_eq!(discovery_hint(0, || None, cut, None), cut);
    assert_eq!(
        discovery_hint(0, || Some(vec![74, 71]), cut, None),
        hint(Some(8), 74)
    );
    assert_eq!(
        discovery_hint(0, || Some(vec![70]), None, Some(75)),
        hint(None, 75)
    );
    assert_eq!(discovery_hint(0, || None, None, None), None);
    let unread = || -> Option<Vec<u64>> { panic!("a store with a floor reads no cache for this") };
    assert_eq!(discovery_hint(72, unread, cut, None), None);
    assert_eq!(discovery_hint(72, unread, cut, Some(73)), hint(None, 73));
}

/// The package log's lanes, read off how the pass was spawned: the head watch's flag, the
/// session's detached spawner, the window's queue-and-report flags, the intercept's queue
/// without a progress file, else typed. And the exit code an `ExitCode` carries comes back
/// out of it.
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

/// A pass-end quotes only the outcome this pass wrote: a row written keeping the outcome
/// (the head-watch door's `update codex` whose vendor was not reached) moves `updated_at`
/// but carries an earlier pass's sentence, which is not this pass's. The same sentence
/// written again is quoted; a pass that wrote none says none.
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

/// `--defer-busy-flip` is the unattended lanes' opt-in to hold the toolchain's flip for
/// quiet: stripped at the edge wherever it sits, never an operand, and without it (a
/// person's `aterm pkg update`) the policy is to flip at once.
#[test]
fn the_defer_busy_flip_flag_is_stripped_and_picks_the_policy() {
    let argv = |parts: &[&str]| parts.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut args = argv(&["update", "--wait-lock", "1800", "--defer-busy-flip"]);
    assert!(super::take_flag(&mut args, super::DEFER_BUSY_FLIP_FLAG));
    assert_eq!(args, argv(&["update", "--wait-lock", "1800"]));
    let mut args = argv(&["update"]);
    assert!(!super::take_flag(&mut args, super::DEFER_BUSY_FLIP_FLAG));
    assert_eq!(args, argv(&["update"]), "absent: untouched");
    // This test process never saw the flag: a person's pass.
    assert_eq!(super::flip_policy(), crate::quiet::FlipPolicy::Now);
}

/// A toolchain flip held for quiet is a state, never a failure: each member it would move
/// gets a `deferred:` row naming the staged build, the live one and the latest it waits
/// until, doctor counts none of it a problem, and the UpToDate arm lifts the row once the
/// wait is over.
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

/// A group pinned mid-wait ends every member's `deferred:` row: the pin ends the held flip,
/// and a sibling the flip would have moved, not pinned itself, stops saying its staged
/// build installs when the toolchain is quiet and says whose pin holds it. A sibling whose
/// row said something else keeps it.
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
    // The owner pins trust-ir mid-wait: the next pass holds the tuple by the pin — a
    // hold, never a failure.
    assert_eq!(
        report_channel_apply(
            &layout,
            &installed,
            &report(crate::TxnOutcome::Pinned(vec!["trust-ir".into()])),
            &[],
            true,
        ),
        0
    );
    assert!(rows("trust-ir").starts_with("held"), "{}", rows("trust-ir"));
    assert!(
        rows("trust").starts_with("held") && rows("trust").contains("trust-ir"),
        "the sibling's deferred row is over, held by trust-ir's pin: {}",
        rows("trust")
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
/// other shape a usage refusal.
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
    for bad in [
        &["d", "make"][..],
        &["d", "--"],
        &["--", "make"],
        &["d", "e", "--", "make"],
        &["d", "--who"],
        &["d", "--who", "--", "make"],
        &["d", "--who", "a", "--who", "b", "--", "make"],
        &["d", "--bogus", "--", "make"],
    ] {
        assert!(parse_lease_args(&argv(bad)).is_err(), "{bad:?} is refused");
    }
    // A blank name is no name: the command's own words name the run.
    let blank = parse_lease_args(&argv(&["d", "--who", " ", "--", "/bin/sh", "-c", "x"])).unwrap();
    assert_eq!(
        lease_who(&blank, 7, std::path::Path::new("/w")),
        "sh -c x (pid 7) \u{2014} held by `aterm pkg lease` in /w"
    );
}

/// The holder holds for exactly the command's life: while the command runs the build it
/// names is leased, by the run's own words, and once it has ended the lease is gone, with
/// the command's status handed back whole. Each step waits on the command's own file,
/// never on a clock.
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
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A directory that is not there once the lease is laid runs nothing (a toolchain
/// reclaimed after its caller resolved it), while one atpkg does not manage runs its
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

/// The holder is transparent: the command it runs holds every descriptor its caller
/// handed the holder (here an unflagged one above 99; in life a make jobserver's pipe or a
/// `3>` redirect) on any std. A `std` child holds only its stdio under Trust's std, so the
/// holder spawns through `aterm_uds::spawnfd::transparent`; its exit status still comes
/// back whole.
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
            // Read from the shell's own table: a `[ -e /dev/fd/N ]` read the held number as
            // absent on Darwin while anything else looked it up.
            aterm_spec::host::held_probe(fd),
        ],
    };
    // No prefix: nothing is leased, and the command runs all the same.
    let run = lease_and_run(None, &args, "w", crate::lease::DEFAULT_WAIT, &mut |_| {});
    let LeaseRun::Ran(status) = run else {
        panic!("the command ran: {run:?}");
    };
    assert_eq!(status.code(), Some(3), "fd {fd} reached the command");
    assert_eq!(crate::platform::exit_code_like(status), ExitCode::from(3));
    drop(held);
}

/// "My window is gone" is a change of parent from the spawner the edge recorded, never
/// "pid 1" on its own: the macOS orphan (launchd) and the Linux subreaper orphan (any
/// other pid) both count; the spawner itself never does, even when it is init (a
/// scheduler's job); a parent nobody can read (Windows) and a spawner nobody named never
/// count, and wait out the bound instead.
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

/// The spawner the edge records: the pid a window names, the parent otherwise, and nobody
/// for a pass detached on purpose (the session's one-shot pass), whose waiter then never
/// reads as orphaned when its detaching `sh` exits.
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

/// A person's terminal gets no line for the window's records or for a fact a sentence
/// beside it already says, a non-marker line passes through, and no marker leaks its
/// raw prefix.
#[test]
fn a_marker_reads_as_a_sentence_on_a_terminal() {
    let said = |line: &str| human_marker(line);
    // Universal Control's change has its own line beside the marker, with the revert.
    let alone = format!(
        "atpkg: {}",
        machine_settings_line(&[crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()])
    );
    assert_eq!(said(&alone), None);
    // `seed-busy:` is silent: the sentence beside it (the intercept's epilogue, or the
    // typed lane's timed-out sentence) already says it.
    for silent in [
        LOCK_ACQUIRED_MARKER,
        GROUP_ABORTED_MARKER,
        SHADOWED_MARKER,
        FOREIGN_MARKER,
        C_TOOLCHAIN_MARKER,
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

use super::*;
use std::os::unix::fs::PermissionsExt;

/// The `install` argv takes one operand: a second is a usage error before any store is
/// touched (`install` parses its own argv, outside the one-operand gate).
#[test]
fn install_takes_one_operand() {
    let two = std::process::ExitCode::from(2);
    assert_eq!(cmd_install_argv(&["ay".to_string(), "ny".to_string()]), two);
}

/// `doctor --json` swallowed the flag and exited 0 with the human report (audit D-12),
/// and `repair --dry-run` rewrote the shell wiring at exit 0 (2026-09-16). A verb whose
/// row says [`Arity::None`] refuses ANY argument at the dispatch edge, before the store
/// lock, naming the verb, the argument and the arity; the doctor takes `--verbose` and
/// `--json` (its rows in the one schema, design §3.3), each once, and nothing else.
/// `--help` still answers through the edge, and the bare forms run.
#[test]
fn zero_arity_verbs_refuse_arguments_instead_of_swallowing_them() {
    let none: Vec<&str> = TABLE
        .iter()
        .filter(|v| v.arity == Arity::None)
        .map(|v| v.name)
        .collect();
    assert_eq!(none, ["gc", "seed"]);
    let json = vec!["--json".to_string()];
    for verb in &none {
        assert_eq!(
            arity_refusal(&json, verb),
            Some(ExitCode::from(2)),
            "{verb} refuses at the usage exit"
        );
        assert!(arity_refusal(&[], verb).is_none(), "bare `{verb}` proceeds");
    }
    // `repair` takes `--json` (its rows in the one schema) once, and nothing else.
    let arg = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
    for refused in [&["--dry-run"][..], &["--json", "--json"], &["--json", "x"]] {
        assert_eq!(
            arity_refusal(&arg(refused), "repair"),
            Some(ExitCode::from(2)),
            "{refused:?}"
        );
    }
    assert!(arity_refusal(&json, "repair").is_none());
    assert!(arity_refusal(&[], "repair").is_none());
    for verb in ["doctor", "status"] {
        use crate::doctor::Detail::{Everything, Problems};
        let arg = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        for refused in [
            &["--verbose", "x"][..],
            &["--jsonl"],
            &["--json", "--json"],
            &["-v", "--verbose"],
        ] {
            assert_eq!(
                doctor_args(&arg(refused), verb).err(),
                Some(ExitCode::from(2)),
                "{refused:?}"
            );
        }
        assert_eq!(
            doctor_args(&arg(&["-v"]), verb).ok(),
            Some((Everything, false))
        );
        assert_eq!(doctor_args(&[], verb).ok(), Some((Problems, false)));
        assert_eq!(
            doctor_args(&arg(&["--json"]), verb).ok(),
            Some((Problems, true))
        );
        assert_eq!(
            doctor_args(&arg(&["--json", "--verbose"]), verb).ok(),
            Some((Everything, true))
        );
    }
    // The process edge (a refused argument exits 2 before the store or the lock) is
    // `tests/one_operand_verbs.rs`'s, over a scratch HOME.
}

/// The one-name gate leaves the multi-name grammars ([`Arity::Own`]) alone (`link
/// <program> <checkout>`, `refresh a b`, `run <tool> <args…>`); its refusals are
/// `tests/one_operand_verbs.rs`'s, through the real binary, and `update`'s `--retry` is
/// counted out in `cli_held_row_tests`.
#[test]
fn the_one_operand_gate_never_refuses_a_multi_name_grammar() {
    let two = vec!["ta".to_string(), "tb".to_string()];
    for verb in ["install", "link", "refresh", "run"] {
        assert!(arity_refusal(&two, verb).is_none(), "{verb}");
    }
}

/// `help <x>` for an `x` that is not a verb fails (exit 2) rather than printing the
/// roster at exit 0, which would read as the help for x.
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
    // Every verb has a usage line by construction: it is a field of its table row.
}

/// The suggestion engine: a one-slip typo gets the fix, gibberish gets silence —
/// a far-fetched guess erodes trust in the near ones.
#[test]
fn did_you_mean_suggests_and_gives_up() {
    assert_eq!(did_you_mean("instal"), Some("install"));
    assert_eq!(did_you_mean("udpate"), Some("update"));
    assert_eq!(did_you_mean("frobnicate"), None);
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

/// `list` with no resolvable prefix exits 1, like every sibling read verb.
#[test]
fn list_exit_codes() {
    assert_eq!(
        format!("{:?}", run_list(None, true)),
        format!("{:?}", ExitCode::from(1)),
        "no layout is a structural failure, not a success"
    );
}

/// Uninstalling what is not there is a refusal: exit 1, and no removed-marker is minted.
/// Partial-install debris (a store tree with no shims) still cleans.
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

/// `uninstall --all` reports only what it removed. The sweep walks the union of the live
/// builds and every status row, and `ops::uninstall` returns Ok for a name with no store
/// tree under the prefix, so a system copy, a row an older client wrote for an OS-level
/// install, and a queued agent program must not count as removals
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

/// Only an agent program's stub passes through, and only to a foreign copy on PATH outside
/// the managed prefix: `trust` never does, and `claude` with no other copy prints its
/// state and stops.
#[cfg(unix)]
#[test]
fn only_an_agent_program_with_a_foreign_copy_passes_through() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = temp_layout("passthrough");
    // Named for this process: the temp dir is shared with other runs of this suite.
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

/// What `repair`'s rc pass will not touch: an rc linked into a read-only directory (a
/// home-manager store's shape) is refused with its target untouched, and a `~/.aterm`
/// that is a symlink touches no rc at all. (The opt-out, and `repair` laying the block
/// back over it, are `hooks.rs`'s `a_deleted_rc_block_stays_deleted_and_repair_is_the_way_back`.)
#[test]
fn repair_refuses_a_read_only_rc_and_a_linked_aterm_dir() {
    use crate::hooks::{HookPass, RcOutcome, RcWiring};
    let layout = temp_layout("repair-rc");
    let home = crate::testdir::dir("atpkg-main-repair-rc-home-");
    let zshrc = home.join(".zshrc");
    let mine = "export FOO=1\n";

    // Non-root only: root can write a 0555 directory, so the refusal never happens.
    if crate::platform::our_uid() != 0 {
        let store = home.join("ro-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("zshrc"), mine).unwrap();
        std::os::unix::fs::symlink(store.join("zshrc"), &zshrc).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
        let ro = crate::hooks::pass_at(&layout, &home, RcWiring::Rewire, None);
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ro.rc(), [(".zshrc", RcOutcome::WriteFailed)]);
        assert_eq!(std::fs::read_to_string(store.join("zshrc")).unwrap(), mine);
    }

    // ~/.aterm refused (a symlink): nothing touched.
    let refused = home.join("refused-home");
    std::fs::create_dir_all(refused.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(refused.join("elsewhere"), refused.join(".aterm")).unwrap();
    std::fs::write(refused.join(".zshrc"), mine).unwrap();
    let pass = crate::hooks::pass_at(&layout, &refused, RcWiring::Rewire, None);
    assert_eq!(pass, HookPass::NotHardened);
    assert_eq!(
        std::fs::read_to_string(refused.join(".zshrc")).unwrap(),
        mine
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// THE FIRST LAUNCH'S TWO `[machine]` APPLIES MEET ON ONE QUEUE. Before any pass has made
/// the store's prefix, the window's (or a session's) launch apply and the launch seed's edge
/// both start: the apply's queue makes the prefix ([`super::machine_apply_queue`]), and the
/// edge, asking while it is held, finds it taken and leaves the table to that apply
/// ([`super::MachineTurn::Busy`]); once it is let go the edge takes it. Negative control: a
/// queue that answers "no prefix, no queue" leaves both to apply unqueued, side by side.
#[test]
fn the_first_launchs_machine_applies_meet_on_one_queue_before_any_prefix_exists() {
    let home = crate::testdir::dir("atpkg-machine-queue-");
    let layout = crate::store::Layout {
        prefix: crate::store::default_prefix(&home),
    };
    assert!(
        !layout.prefix.exists(),
        "precondition: a first launch, no prefix yet"
    );
    let launch_apply = super::machine_apply_queue_in(&layout)
        .expect("the launch apply's queue makes the prefix and takes the queue");
    assert!(
        matches!(
            super::machine_apply_turn_in(&layout),
            super::MachineTurn::Busy
        ),
        "the seed's edge finds the queue taken"
    );
    drop(launch_apply);
    assert!(
        matches!(
            super::machine_apply_turn_in(&layout),
            super::MachineTurn::Mine(_)
        ),
        "and takes it once it is let go"
    );
}

fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-main-{label}-"))
}

/// A command nothing here runs is answered by the program that ships it: its recorded
/// reason when no install changes it (no build for this target, a hold), else that
/// program's install — never `aterm pkg install targo`, which install refuses, nor an
/// install that answers "unavailable". A program asks of its own row.
#[test]
fn a_command_nothing_runs_is_answered_by_the_program_that_ships_it() {
    let unavailable = crate::state::unavailable("aarch64-unknown-linux-gnu", "");
    assert_eq!(
        not_runnable_words("targo", "trust", None),
        "atpkg: targo comes with trust, which is not installed (fix: aterm pkg install trust)"
    );
    assert_eq!(
        not_runnable_words("targo", "trust", Some(&unavailable)),
        format!("atpkg: targo comes with trust: {unavailable}")
    );
    assert_eq!(
        not_runnable_words("ay", "ay", Some(&unavailable)),
        format!("atpkg: ay: {unavailable}")
    );
    assert_eq!(
        not_runnable_words("ty", "ty", None),
        not_installed_fix("ty")
    );

    // Through the record: the program's row decides, the command's name has none.
    let layout = temp_layout("not-runnable");
    std::fs::create_dir_all(&layout.prefix).unwrap();
    assert_eq!(
        not_runnable_line(&layout, "trustc"),
        not_runnable_words("trustc", "trust", None)
    );
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        crate::ProgramStatus {
            installed_build: None,
            state: unavailable.clone(),
            tree_root: String::new(),
        },
    );
    crate::status::write(
        &layout,
        &crate::Status {
            schema: 1,
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        not_runnable_line(&layout, "trustc"),
        format!("atpkg: trustc comes with trust: {unavailable}")
    );
    assert_eq!(
        not_runnable_line(&layout, "trust"),
        format!("atpkg: trust: {unavailable}")
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A parsed index shaped by hand: `(name, system)` per program. No channels: the
/// set-planning helpers under test read `[programs]` only.
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

fn names(v: &[&str]) -> std::collections::BTreeSet<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

// `which <program>` for a bundle whose build exposes no command of its own name (`trust`
// → trustc/targo, `trust-mc` → trust-mc-driver) answers with the managed store dir and
// the names it does put on PATH; a same-named unrelated binary on PATH (Homebrew's
// p11-kit `trust`) is named as exactly that, after the truth, never as "the system copy".
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
    // A same-named unrelated binary on PATH (p11-kit's `trust`).
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
    let first = lines.next().unwrap();
    assert!(
        first.contains(&build.display().to_string()) && first.contains("tm-driver"),
        "the managed store dir and the tool it exposes lead: {first}"
    );
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with(&brew.join("tm").display().to_string()),
        "the same-named binary on PATH is named after the truth: {line}"
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
/// version its real location spells; nothing is executed to learn it.
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
    // The self-update sentence (`crate::selfupdate`) closes the managed line, ahead of
    // the foreign-copies line.
    assert_eq!(
        lines.next().unwrap(),
        // agents/ first: the twin is the copy that runs, and what the closing sentence
        // is about.
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
            // Home-abbreviated: a TMPDIR under $HOME puts this scratch root there.
            super::home_abbreviated(&local.join("claude")),
            super::home_abbreviated(&brew.join("claude"))
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
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `which claude` in a shell whose reroute stub decides at exec time: inside aterm the
/// stub takes the twin, so the line names that route (stub, twin, store) with the
/// self-update sentence the twin makes true, whatever this shell's PATH puts ahead;
/// outside aterm the stub passes through to this shell's own copy, said as the design
/// (never SHADOWED, no remedy), and where the pass-through reaches `bin/` the managed line
/// carries no self-update sentence.
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
    // …and so is an iTerm shell's, whose PATH has no reroute dir at all: the stub is
    // laid, this shell's own copy runs by design, no SHADOWED, no remedy.
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
    // `agents/` on it but skipped by the stub), the managed line stands without the
    // self-update sentence: `bin/claude` carries no hand-over, and the twin that does is
    // not what runs there.
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

/// The version a foreign copy's path spells: the last dotted-digits component, or nothing,
/// never a guess.
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

// A failed Universal Control write is reported under `MACHINE_APPLY_FAILED_PREFIX` and
// mints no `machine-settings:` entry; only the success arm adds
// `universal-control disabled`, with its revert.
#[test]
fn machine_write_failure_is_explicit() {
    use crate::machine::UniversalControlOutcome;
    let mut entries: Vec<String> = Vec::new();
    let failure =
        record_universal_control_outcome(UniversalControlOutcome::WriteFailed, &mut entries)
            .expect("a failed write must be reported even when the verb exits 0");
    assert!(
        failure.starts_with(MACHINE_APPLY_FAILED_PREFIX),
        "{failure}"
    );
    assert!(
        entries.is_empty(),
        "a failed Universal Control write cannot mint its success event"
    );
    for outcome in [
        UniversalControlOutcome::NotApplicable,
        UniversalControlOutcome::AlreadyDisabled,
    ] {
        assert!(record_universal_control_outcome(outcome, &mut entries).is_none());
        assert!(entries.is_empty());
    }
    // The genuine success arm remains distinct and carries its revert.
    let success =
        record_universal_control_outcome(UniversalControlOutcome::Disabled, &mut entries).unwrap();
    assert!(!success.starts_with(MACHINE_APPLY_FAILED_PREFIX));
    assert!(success.contains(crate::machine::UNIVERSAL_CONTROL_REVERT));
    assert_eq!(
        entries,
        [crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()]
    );
}

/// A Rosetta 2 install is a `machine-settings:` entry only when it landed; a failure is
/// the failed line with atpkg's why, and an installed or inapplicable Mac says nothing.
#[test]
fn a_rosetta_install_is_an_entry_only_when_it_landed() {
    use crate::machine::RosettaOutcome;
    let mut entries: Vec<String> = Vec::new();
    let failure = record_rosetta_outcome(
        RosettaOutcome::Failed("softwareupdate exited 1: no network".into()),
        &mut entries,
    )
    .expect("a failed install is reported even when the verb exits 0");
    assert!(
        failure.starts_with(MACHINE_APPLY_FAILED_PREFIX),
        "{failure}"
    );
    assert!(failure.contains("no network"), "{failure}");
    assert!(entries.is_empty());
    for outcome in [
        RosettaOutcome::NotApplicable,
        RosettaOutcome::AlreadyPresent,
    ] {
        assert!(record_rosetta_outcome(outcome, &mut entries).is_none());
        assert!(entries.is_empty());
    }
    let installed = record_rosetta_outcome(RosettaOutcome::Installed, &mut entries).unwrap();
    assert!(!installed.starts_with(MACHINE_APPLY_FAILED_PREFIX));
    assert!(installed.contains("rosetta = false"), "{installed}");
    assert_eq!(entries, [crate::machine::ROSETTA_ENTRY.to_string()]);
    assert_eq!(
        machine_settings_line(&[
            crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string(),
            crate::machine::ROSETTA_ENTRY.to_string(),
        ]),
        "machine-settings: universal-control disabled; rosetta installed"
    );
}

/// Bare `machine`'s verdict names each half an apply would do, and says nothing is left
/// in the words of what applies here; `nothing changed` blames the switches only when
/// every setting that applies is switched off.
#[test]
fn the_machine_verdicts_name_what_is_left_and_why_nothing_changed() {
    use crate::config::UniversalControlPolicy::Off;
    use crate::machine::{HomePosture, MachineState, RosettaPosture, UcPosture};
    let at = |universal_control, rosetta| MachineState {
        universal_control,
        policy: Off,
        home: HomePosture::Account,
        config_unreadable: false,
        rosetta,
        rosetta_wanted: true,
    };
    assert_eq!(
        machine_read_verdict(&at(UcPosture::Default, RosettaPosture::Missing), None),
        "next — aterm pkg machine apply (Universal Control off for this host; Rosetta 2 \
         installed)"
    );
    assert_eq!(
        machine_read_verdict(&at(UcPosture::Disabled, RosettaPosture::Missing), None),
        "next — aterm pkg machine apply (Rosetta 2 installed)"
    );
    assert_eq!(
        machine_read_verdict(&at(UcPosture::Disabled, RosettaPosture::Present), None),
        "nothing to apply — Universal Control and Rosetta 2 are where [machine] wants them"
    );
    assert_eq!(
        machine_read_verdict(
            &at(UcPosture::Disabled, RosettaPosture::NotApplicable),
            None
        ),
        "nothing to apply — Universal Control is where [machine] wants it",
        "an Intel Mac's words are the ones before Rosetta 2"
    );
    let unread = machine_read_verdict(&at(UcPosture::Disabled, RosettaPosture::Unknown), None);
    assert!(
        unread.starts_with("nothing to apply — ") && !unread.contains("Rosetta 2 are where"),
        "a runtime nobody could read is not vouched for: {unread}"
    );
    assert!(unread.contains("Rosetta 2 could not be read"), "{unread}");
    assert_eq!(
        machine_read_verdict(
            &at(UcPosture::Default, RosettaPosture::Missing),
            Some("HOME is /tmp/x".into())
        ),
        "not applied here — HOME is /tmp/x"
    );
    let left = MachineApply {
        left_alone: vec!["universal_control = \"leave\"", "rosetta = false"],
        ..MachineApply::default()
    };
    assert_eq!(
        left.nothing_changed(),
        "nothing changed — [machine] universal_control = \"leave\", rosetta = false"
    );
    let rosetta_on = MachineApply {
        left_alone: vec!["universal_control = \"leave\""],
        acts: true,
        ..MachineApply::default()
    };
    assert_eq!(
        rosetta_on.nothing_changed(),
        "nothing changed — already applied",
        "Rosetta 2 was already there: the switch is not the cause"
    );
    let failed = MachineApply {
        failed: true,
        ..rosetta_on
    };
    assert_eq!(
        failed.nothing_changed(),
        "nothing changed — a change did not land"
    );
}

/// The marker LINES are a cross-crate contract (`crates/aterm-gui` splits them; the
/// marker constants themselves are shared, so only the composed payloads are pinned
/// here): `machine-settings:`'s entry, `foreign:`'s items with a `; ` inside a field
/// written `, `, and `managed-current:`'s entries the GUI splits on `; `.
#[test]
fn the_marker_payloads_are_byte_stable() {
    assert_eq!(
        machine_settings_line(&[crate::machine::UNIVERSAL_CONTROL_ENTRY.to_string()]),
        "machine-settings: universal-control disabled"
    );
    assert_eq!(
        foreign_line(FOREIGN_SEAM, &[]),
        "foreign: seam none",
        "a look that found nothing says so"
    );
    assert_eq!(
        foreign_line(
            FOREIGN_PATH,
            &[
                ForeignItem {
                    name: "trust".into(),
                    path: "/u/.local/bin/trustdoc".into(),
                    fix: "type alab-trustdoc for the managed one".into(),
                },
                ForeignItem {
                    name: "gh".into(),
                    path: "/u/odd; dir/gh".into(),
                    fix: "type alab-gh for the managed one".into(),
                },
            ]
        ),
        "foreign: path trust /u/.local/bin/trustdoc — type alab-trustdoc for the managed one; \
         gh /u/odd, dir/gh — type alab-gh for the managed one",
        "items split back whole: a `; ` inside a field is written `, `"
    );
    assert_eq!(
        managed_current_line(&[]),
        None,
        "omitted when no agent program is installed"
    );
    let agent = |name: &str, version: &str, standing: &str| ManagedAgent {
        name: name.into(),
        version: version.into(),
        standing: standing.into(),
    };
    assert_eq!(
        managed_current_line(&[
            agent("claude", "2.1.281", "Anthropic latest"),
            agent("codex", "0.156.0", "OpenAI latest"),
        ])
        .as_deref(),
        Some("managed-current: claude 2.1.281 (Anthropic latest); codex 0.156.0 (OpenAI latest)")
    );
}

/// THE MANAGED-CURRENT LINE NAMES VENDOR PROGRAMS (design §1.7): an agent program whose
/// active vendor build's row says it is the vendor's latest, and whose twin is that
/// build, is listed by version and source — never a store id, never an index pin. One
/// whose vendor this run could not reach, or no check has reached for a day, is listed
/// `not checked`, and a kept row by its own word (audit 2026-10-09: an unreached
/// Anthropic left the line naming Codex alone, read as every program current); a row
/// for another build and a missing twin each leave it out.
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
    // Unreachable this run: the row is an older check's, so the line says `not checked`
    // rather than leaving the program out.
    let unreached = std::collections::BTreeSet::from([String::from("claude")]);
    assert_eq!(
        managed_current_line(&managed_current_entries(&layout, &unreached, now)).as_deref(),
        Some("managed-current: claude 2.1.281 (not checked)")
    );
    // Nor current when the last check that reached the vendor is more than a day old.
    assert_eq!(
        managed_current_line(&managed_current_entries(
            &layout,
            &none,
            now + 24 * 60 * 60 + 1
        ))
        .as_deref(),
        Some("managed-current: claude 2.1.281 (not checked)"),
        "a stale check"
    );
    // A kept row says its own word, reached or not.
    record_status(
        &layout,
        "claude",
        row(
            crate::state::vendor_kept("2.1.281", "held by local pin"),
            claude,
        ),
        "x".into(),
    );
    for unchecked in [&none, &unreached] {
        assert_eq!(
            managed_current_line(&managed_current_entries(&layout, unchecked, now)).as_deref(),
            Some("managed-current: claude 2.1.281 (held)")
        );
    }
    // A row for another build is not what runs: nothing.
    let other = row(
        crate::state::vendor_managed("2.1.280", "Anthropic"),
        claude - 1,
    );
    record_status(&layout, "claude", other, "x".into());
    assert_eq!(managed_current_entries(&layout, &none, now), Vec::new());
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
}

/// The one remedy: where the hook file for the invoking shell exists (the parent
/// process's, `caller_shell`; `$SHELL` only when that is no shell) it is sourced in place
/// (`. ~/.aterm/shell.d/00-atpkg.zsh`, `source …fish`, the dot-source for pwsh), wired rc
/// or not, and never `exec $SHELL`, which inside an aterm tab drops the tab's shell
/// integration (the zsh wrapper consumes `ATERM_ORIGINAL_ZDOTDIR`, so a re-exec'd zsh
/// loads none). Only a shell without the hook file gets a PATH line, in its own dialect.
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
    let _ = std::fs::remove_dir_all(&home);
}

/// Every rustup seam re-assertion ends with its [`FOREIGN_MARKER`] line, naming a foreign
/// `trust` entry with [`crate::seam::DETACH_FIX`] and leaving the entry alone. Negative
/// control: once the entry is gone, and with no rustup home at all, the line says `none`.
#[cfg(unix)]
#[test]
fn the_seam_reassertion_names_a_foreign_entry_on_the_foreign_line() {
    let layout = temp_layout("seamforeign");
    let build = layout.build_dir("trust", 6808);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::create_dir_all(build.join("lib").join("rustlib")).unwrap();
    crate::activate::atomic_symlink(&build, &crate::seam::store_current(&layout)).unwrap();
    let rustup = layout.prefix.join("rustup-home");
    std::fs::create_dir_all(crate::seam::toolchains_dir(&rustup)).unwrap();
    let elsewhere = layout.prefix.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let seam = crate::seam::seam_path(&rustup, "trust");
    std::os::unix::fs::symlink(&elsewhere, &seam).unwrap();
    let _ = take_markers_said();
    reassert_rustup_seam_at(&layout, Some(&rustup));
    assert_eq!(
        take_markers_said(),
        vec![format!(
            "atpkg: foreign: seam trust {} — {}",
            seam.display(),
            crate::seam::DETACH_FIX
        )]
    );
    assert_eq!(std::fs::read_link(&seam).unwrap(), elsewhere, "left alone");
    std::fs::remove_file(&seam).unwrap();
    reassert_rustup_seam_at(&layout, Some(&rustup));
    assert_eq!(
        take_markers_said(),
        vec!["atpkg: foreign: seam none".to_string()],
        "the entry gone: the window ends its entry"
    );
    reassert_rustup_seam_at(&layout, None);
    assert_eq!(
        take_markers_said(),
        vec!["atpkg: foreign: seam none".to_string()],
        "no rustup home: nothing can stand there"
    );
}

/// `atpkg which` (design S6) and the shadow reconcile (design S5), over a real store: a
/// managed shim answers `managed <build> — pinned by index <N>` with the shim → store
/// resolution; a foreign copy ahead of the managed bin/ turns that into `SHADOWED by
/// <path>` (said by the pass, never fixed); a foreign copy with no managed shim is the
/// `system:` state; a pending stub says the next pass installs it; nothing is the fix
/// line.
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
    // A foreign copy ahead of the managed bin/.
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
    // trust is ALab's own, so its alias is laid: the answer carries the one trailing
    // fix-line after the canonical state (§17.11).
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
    // A foreign `alab-trust` ahead of the managed bin/ too: the alias would not run the
    // managed copy either, so the fix-line is withheld and `which alab-trust` reports the
    // alias as SHADOWED itself.
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
        // The pass still says the shadow, but a shadow with no fix is no exception: the
        // window's foreign line names nothing. Negative control: the fixed shadow below,
        // which the line names.
        let _ = take_markers_said();
        assert_eq!(
            reconcile_shadowed(&layout, 41, Some(&ahead)),
            vec!["trust".to_string()]
        );
        assert!(
            take_markers_said().contains(&"atpkg: foreign: path none".to_string()),
            "a shadow with no fix is not on the foreign line"
        );
        std::fs::remove_file(&foreign_alias).unwrap();
    }
    // The containment is anchored: an `alab-<x>` that resolves into a `store/<x>/…` tail
    // outside this prefix (a hand-made link into a checkout) earns no fix-line.
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
    // The pass says it and records nothing (SHADOWED is one shell's truth); a reader with
    // that PATH computes it: the canonical row, never the fix-line.
    let before = crate::status::read(&layout).unwrap().programs["trust"]
        .state
        .clone();
    let _ = take_markers_said();
    assert_eq!(
        reconcile_shadowed(&layout, 41, Some(&ahead)),
        vec!["trust".to_string()]
    );
    // …and names it for the window, with the fix the line beside it says.
    assert!(
        take_markers_said().contains(&format!(
            "atpkg: foreign: path trust {} — type alab-trust for the managed one",
            local.join("trust").display()
        )),
        "the foreign line names the copy and its fix"
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
        take_markers_said(),
        vec!["atpkg: foreign: path none".to_string()],
        "a look that finds nothing says so, so the window can end what it no longer sees"
    );
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
    // SHADOWED), and names it on no `shadowed:` marker.
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
    // this shell's own copy, a note.)
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
    // The pass: nothing shadowed, the managed row recorded, the tail printed.
    let _ = take_markers_said();
    assert!(
        !reconcile_shadowed(&layout, 41, Some(&no_agents)).contains(&"claude".to_string()),
        "an agent program with a current twin is never on the shadowed marker"
    );
    assert!(
        take_markers_said()
            .iter()
            .any(|l| l.starts_with("atpkg: foreign: path ") && !l.contains(" claude ")),
        "nor on the foreign line: nothing there is the owner's to fix"
    );
    assert!(
        !crate::status::read(&layout)
            .and_then(|s| s.programs.get("claude").cloned())
            .is_some_and(|r| crate::state::shadowed_by(&r.state).is_some()),
        "never recorded as SHADOWED"
    );
    // …and a row an older client recorded as SHADOWED is put back to managed: for a
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
    // With the twin gone (a lane that could not lay it), the plain SHADOWED row is the
    // truth again for that PATH: nothing in front of it vouches for the managed copy.
    // Said by the pass, and read, never recorded.
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
    // A pending stub: the next pass installs it.
    crate::stub::write_pending_stub(&layout, &crate::store::ToolName::new("emacs").unwrap())
        .unwrap();
    let line = which_line(&layout, "emacs", Some(&managed_only)).unwrap();
    assert!(line.starts_with("emacs → "), "{line}");
    assert!(line.contains("(pending stub)"), "{line}");
    // A program that is both a pending stub and shadowed by a foreign copy: PATH order
    // decides what runs. Foreign copy ahead of the managed bin/ ⇒ it runs and atpkg does
    // not manage it; behind it ⇒ the stub runs and waits.
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
    // A command's stub names the program whose install lays it.
    crate::stub::write_pending_stub(&layout, &crate::store::ToolName::new("targo").unwrap())
        .unwrap();
    let line = which_line(&layout, "targo", Some(&managed_only)).unwrap();
    assert!(line.starts_with("targo → "), "{line}");
    assert!(line.ends_with("(now: aterm pkg install trust)"), "{line}");
    let _ = std::fs::remove_file(layout.shim(&crate::store::ToolName::new("targo").unwrap()));
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
    // no alias instead of promising `aterm pkg install alab-x`. claude is installed
    // above with the policy Off; with no cached index the line can
    // only say the tool is installed and not aliased. An unknown base gets the plain
    // name's fix line.
    assert!(which_line(&layout, "alab-claude", Some(&managed_only)).is_err());
    assert!(which_line(&layout, "alab-nothing", Some(&managed_only)).is_err());
    // A name the cached index does not serve: install's own refusal, never a promise
    // of an install that refuses. A served name, a vendor program, and no cached
    // index at all keep the fix line.
    let index = index_of(&[("ay", None), ("gh", Some("gh")), ("codex", None)]);
    assert!(unserved_which_line("nosuch", Some(&index)).is_some());
    assert_eq!(unserved_which_line("ay", Some(&index)), None);
    assert_eq!(unserved_which_line("claude", Some(&index)), None);
    assert_eq!(unserved_which_line("nosuch", None), None);
    assert!(which_line(&layout, "alab-sudo", Some(&managed_only)).is_err());
    assert!(which_line(&layout, "../x", Some(&managed_only)).is_err());
    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `which trust` with p11-kit's `trust` on PATH, through the store, in the two states the
/// fix for the cached-index case missed: a fresh machine with NO index cached, and a
/// seeded one whose pending stub sits behind the unrelated binary (the hook APPENDS bin/
/// to PATH) — each names the binary as unrelated, never "system: … not managed by
/// aterm". A pending stub AHEAD of it is what runs, and says so.
#[cfg(unix)]
#[test]
fn which_names_a_same_named_binary_unrelated_with_no_index_or_behind_a_stub() {
    let layout = temp_layout("p11-kit");
    std::fs::create_dir_all(layout.bin_dir()).unwrap();
    // Outside the prefix: every directory under it is the store's, never foreign.
    let foreign_dir = std::path::PathBuf::from(format!("{}-usr-bin", layout.prefix.display()));
    std::fs::create_dir_all(&foreign_dir).unwrap();
    std::fs::write(foreign_dir.join("trust"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        foreign_dir.join("trust"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    let path = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();

    // No index, no stub: the compiled roster says `trust` is ALab's own.
    let line = which_line(&layout, "trust", Some(&path(&[&foreign_dir]))).unwrap();
    assert!(
        line.starts_with("trust → (nothing runs) — not installed"),
        "{line}"
    );
    assert!(line.contains("on PATH is an unrelated binary"), "{line}");
    assert!(!line.contains("system:"), "{line}");

    // A pending stub BEHIND the foreign binary: still unrelated, with the stub's words.
    crate::stub::write_pending_stub(&layout, &crate::store::ToolName::new("trust").unwrap())
        .unwrap();
    let behind = path(&[&foreign_dir, &layout.bin_dir()]);
    let line = which_line(&layout, "trust", Some(&behind)).unwrap();
    assert!(
        line.contains("not installed yet: the next pass installs it"),
        "{line}"
    );
    assert!(line.contains("on PATH is an unrelated binary"), "{line}");
    assert!(!line.contains("system:"), "{line}");

    // AHEAD of it, the stub is what runs (arm 3), as before.
    let ahead = path(&[&layout.bin_dir(), &foreign_dir]);
    let line = which_line(&layout, "trust", Some(&ahead)).unwrap();
    assert!(line.contains("(pending stub)"), "{line}");
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&foreign_dir);
}

/// The rolled-back row claims no pin: an index-program rollback records
/// `managed <to> — rolled back from <from>`, never a `pinned by index N` for a build index
/// N does not pin, and `which`'s state reader prints those words for that build.
#[test]
fn a_rolled_back_row_records_what_happened_and_which_prints_it() {
    let words = crate::state::rolled_back(7345, 8606);
    assert_eq!(words, "managed 7345 — rolled back from 8606");
    assert!(crate::state::is_managed(&words));
    assert_eq!(crate::state::managed_pin(&words), None, "no pin is claimed");
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

/// The set's need is the peak the per-unit gates enforce, archives included, so freeing
/// exactly the shortfall the line names lets the tuple's own gate pass. The rustc tuple
/// alone (trust 3 GiB + a 1 GiB archive, three 100 MiB siblings with 50 MiB archives) on
/// 4 GiB free needs 5.4 GiB, where installed sizes alone sum to 4.3 GiB.
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
    let r = set_disk_refusal(&units, 0, 4 * GIB, prefix).expect("refuses");
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
    assert!(set_disk_refusal(&units, 0, need, prefix).is_none());
    assert!(crate::cost::disk_ok(
        rustc.bytes.need(),
        need,
        crate::cost::FREE_FLOOR
    ));
    assert!(set_disk_refusal(&units, 0, need - 1, prefix).is_some());
}

/// Agents landing beside the set are in its figure: a set that fits by itself, beside
/// agents that need 2 GiB at once and fit by themselves, is refused when the volume cannot
/// hold both at once (the line names the joint need and why) and passes at exactly that
/// need.
#[test]
fn default_set_disk_refusal_counts_the_agents_landing_beside_it() {
    let prefix = std::path::Path::new("/Users//x/.aterm");
    let units = [unit(None, &[("ay", 2 * GIB, GIB)])];
    let set = crate::flow::peak_need(units.iter().map(|u| u.bytes));
    let have = set + crate::cost::FREE_FLOOR + GIB;
    assert!(
        set_disk_refusal(&units, 0, have, prefix).is_none(),
        "the set fits alone"
    );
    assert!(
        crate::cost::disk_ok(2 * GIB, have, crate::cost::FREE_FLOOR),
        "and so do the agents"
    );
    let r = set_disk_refusal(&units, 2 * GIB, have, prefix).expect("not both at once");
    let need = crate::cost::human_bytes(set + 2 * GIB + crate::cost::FREE_FLOOR);
    assert!(
        r.line.contains(&format!(
            "needs {need} with the agents installing beside it"
        )),
        "{}",
        r.line
    );
    let joint = set + 2 * GIB + crate::cost::FREE_FLOOR;
    assert!(set_disk_refusal(&units, 2 * GIB, joint, prefix).is_none());
    assert!(set_disk_refusal(&units, 2 * GIB, joint - 1, prefix).is_some());
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
    let r = set_disk_refusal(&[ay.clone(), trust], 0, 4 * GIB, prefix).expect("refuses");
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
    let r = set_disk_refusal(&[rustc.clone(), ty], 0, 4 * GIB, prefix).expect("refuses");
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
    let r = set_disk_refusal(&[small, trust.clone()], 0, 4 * GIB, prefix).expect("refuses");
    assert!(!r.line.contains("aterm pkg install"), "{}", r.line);
    assert_eq!(
        r.rows[2],
        (
            "trust".to_string(),
            gate_sentence(trust.bytes.need(), 4 * GIB)
        )
    );
}

/// Every member the refused set would add gets a current row, so a stale figure from an
/// earlier pass cannot outlive it: three programs that each fit alone, refused on their
/// sum, all record the set's sentence, and a member held by its memo keeps its held row.
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
    let r = set_disk_refusal(&units, 0, 5 * GIB, prefix).expect("7 GiB on 5 GiB refuses");
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

/// The units the set check judges: a unit its lane lands nothing of is left out (on
/// 4.5 GiB free, a 4 GiB tuple a sibling's local pin holds does not refuse a 300 MiB
/// program, nor does a held one), while any other unknown leaves the check unanswered. A
/// tuple's unit carries the members the pass adds and the bytes its own gate sums, which
/// can count more than those members.
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
    assert!(set_disk_refusal(&units, 0, have, prefix).is_none());
    // The same tuple counted: the set refuses — what leaving it out spares.
    let units = set_units(&planned, lands(bytes(4 * GIB))).expect("all known");
    assert!(set_disk_refusal(&units, 0, have, prefix).is_some());
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

/// A coherence-group member is held by any member's pin: the update pass's group gate
/// freezes the whole tuple ([`crate::flow::pin_gate_members`]). So the line names every
/// unpin the hold takes (the program's own when it is pinned, and each pinned sibling's)
/// and claims nothing about updates moving it while a sibling's pin holds the tuple; with
/// no cached index to read the group from, an unchanged pin predicts nothing.
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

/// THE SELF-UPDATE VERB (2026-09-19, `crate::selfupdate`), in-process, the endings the
/// binary-level `tests/selfupdate_intercept.rs` does not reach: usage exit 2 (a `--help`
/// operand too — non-mutating); not a tool name exit 2; the prefix cross-check proceeds
/// when the store atpkg resolves here agrees with the operand or cannot be resolved at
/// all; `install stable` and `install 2.1.200` are declined whether the manager is
/// enabled or not — there is no escape hatch, the plan has no input that could turn a
/// decline into a forward — and `update --foo` is declined; the disabled manager refuses
/// with its line for every check shape; the hidden verb takes no lock. Then THE 75
/// ENDING's relay (§8): `selfupdate_check` with a ONE-SECOND bound runs a stub `atpkg`
/// that records its argv (`update claude --wait-lock 1`) and exits 75, and the verb
/// relays `CONTENDED_EXIT`.
#[cfg(unix)]
#[test]
fn cmd_selfupdate_refuses_forwards_and_never_takes_the_lock() {
    use crate::selfupdate::{self, Decline};
    let layout = temp_layout("selfupdate");
    let claude = crate::store::ToolName::new("claude").unwrap();
    let row = selfupdate::row_for("claude").unwrap();
    let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
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
    // 3. The prefix cross-check proceeds when it agrees or cannot resolve.
    for resolved in [Some(&*layout), None] {
        match selfupdate_plan(&layout, &claude, &args(&["update"]), resolved, true) {
            SelfUpdatePlan::Check { row: r } => {
                assert_eq!(r.program, "claude");
            }
            other => panic!("{resolved:?}: {other:?}"),
        }
    }
    // 4. Declined shapes. No input of the plan can turn a decline into a forward.
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
    assert!(
        matches!(
            selfupdate_plan(
                &layout,
                &claude,
                &args(&["update", "--foo"]),
                Some(&layout),
                true,
            ),
            SelfUpdatePlan::Refuse(_)
        ),
        "an unknown shape is declined"
    );
    // 6. The manager disabled: its line for every check shape.
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
    // 7. The verb takes no lock of its own.
    assert!(!verb_mutates_store(selfupdate::HIDDEN_VERB));
    assert!(
        !layout.store_lock().exists(),
        "no ending above touches the store lock — only the child does"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    // 8. THE 75 ENDING's relay. The child is a stub named `atpkg` (so `child_argv`
    // hands it the argv verbatim) that records that argv and exits 75; the real
    // binary's wait, its lock sentence and its `seed-busy:` marker are
    // `tests/store_lock_wait.rs`'s (`a_waiting_seed_times_out_with_exit_75`).
    let home = crate::testdir::dir("atpkg-main-selfupdate-75-");
    let held = crate::store::Layout {
        prefix: crate::store::default_prefix(&home),
    };
    let wrapper_dir = home.join("wrapper");
    std::fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper = wrapper_dir.join("atpkg");
    let child_argv = home.join("child.argv");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit {}\n",
            child_argv.display(),
            crate::lock::CONTENDED_EXIT
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let code = selfupdate_check(&held, row, &wrapper, 1);
    assert_eq!(
        code,
        ExitCode::from(crate::lock::CONTENDED_EXIT),
        "the child's 75 relayed"
    );
    assert_eq!(
        std::fs::read_to_string(&child_argv).unwrap_or_default(),
        "update\nclaude\n--wait-lock\n1\n",
        "the standard update with the test's own bound"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The predicate behind the `which` sentence, over the bytes every twin renderer lays and
/// the twins older clients laid (pure and cfg-free, so the Windows shape is pinned from
/// here): the `sh` twin with the block carries the hand-over, as does the older
/// block-then-landing twin; the landing-only `sh` twin, the `bin/` shim and the `.cmd`
/// twin (whose self-update prelude renders empty, so on Windows `claude update` is the
/// vendor's) do not. That keeps `aterm pkg which claude` from claiming `here runs …` on
/// Windows.
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
    // The `.cmd` twin renders no block (`twin_prelude`'s Windows arm is empty), so the
    // sentence is never claimed on Windows — nor over an older twin's landing block.
    assert!(!twin_carries_hand_over(
        crate::platform::cmd_shim_content_twin(cmd_target, &env, "").as_bytes()
    ));
    let older_cmd_prelude =
        crate::platform::cmd_landing_prelude("claude", cmd_prefix, cmd_marker, cmd_atpkg);
    assert!(!twin_carries_hand_over(
        crate::platform::cmd_shim_content_twin(cmd_target, &env, &older_cmd_prelude).as_bytes()
    ));
    assert!(!twin_carries_hand_over(b""));
}

/// System satisfaction: a `system = "gh"` member with `gh` on PATH outside the prefix is
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
    // The retirement is dated: the marker holds today, and the row says so.
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

/// A verdict the authority never gave never becomes a row: `NotReachable` means the signed
/// index verified and does not name the token, a fact about the request; recording it
/// would invent a program that `atpkg doctor` reads back as a missing member.
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
    // The offline family, all raised before the program lookup: recorded against a
    // program they would call it broken, when the machine could not reach the channel.
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
    // …while a genuine per-program fact about this machine still lands on the record:
    // no row for this target is the canonical `unavailable on` state, never an error.
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
    // A refused vendor row is a fact about THIS machine's install attempt (the row was
    // signed and reached the client): it lands like any other stage fault.
    assert!(
        failed_install_state(&crate::FlowError::VendorRefused("url host is evil".into()))
            .is_some_and(|s| s.starts_with("error: ") && s.contains("url host is evil"))
    );
}

/// Put `program` genuinely live in `layout` at `build` (real build tree, real shims, real
/// channel pointer), so `active_builds` reports it the way it does on a machine.
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

/// Roll forward: a pass killed between a `current` flip and its shims leaves a complete
/// build no shim runs; the next pass's edge lays its shims from the build and its
/// sidecars, with no fetcher, so nothing is staged or downloaded. A program the machine
/// removed, and an index program whose group membership no cached index can vouch for (a
/// lone member of a tuple must not be split off), are left to the pass; a consistent store
/// rolls nothing.
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

/// The row follows the roll-forward, and an old root never attests a new build: ay 17 live
/// with its signed root, a pass flipped `current` to 18 and was killed before the shims.
/// The next pass's edge rolls ay forward and records 18 with no root, its apply finds ay up
/// to date, and its root recovery re-derives 18's signed root, so `verify` matches. The
/// same holds when the row was lost: a row still naming 17 never carries 17's root onto 18.
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

/// Roll-forward activates nothing the normal path would refuse: a pinned program, a build
/// the latest verified index yanked, and, after an `atpkg rollback`, the build rolled off
/// (the undo flips `current` before it re-lays the shims, so the one divergence it can
/// leave points at the prior build).
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

/// The build a pass keeps below the live one is the build `aterm pkg rollback` lands on:
/// with ay 16, 17 and 18 retained, 18 live and 17 yanked, the pass's closing gc keeps 16
/// and reclaims 17.
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

/// A failed install uninstalls nothing, so it does not report that it did: an upgrade that
/// dies at download/stage/activate leaves the previous build live, and its row keeps
/// `installed_build` and `tree_root`. `crate::verify` fails closed on an empty root, so
/// dropping it would turn one failed upgrade into a permanent "cannot verify".
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

    // The record disagrees with the store: live at 20 while the record says 18/root18.
    // Build from the store and root from the record would claim "build 20, attested by
    // build 18's signature", which disarms verify's `BuildMismatch` guard and yields a
    // false `Drift` or, worse, a false `Match`.
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

/// A tombstoned program keeps its attestation: its shims are failing stubs, so
/// `active_builds` goes silent, but its bytes and signed root sit untouched in the store,
/// and the build-agreement rule above must not read that silence as disagreement.
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

/// A complete build dir with an executable `bin/<program>` and its ready marker (the shape
/// `list_installed` reports), for the repair tests below.
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

/// Repair never revives a revoked build. A tombstone keeps the revoked build's tree,
/// marker and `current` links; only its shim becomes a failing notice that resolves to
/// nothing, and repair must not read that as "no exposes recorded" and lay a live shim
/// over it. Refused both on the recorded `tombstoned:` row and on the tombstone shim alone.
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

/// A lay that fails reaches the exit code: every program whose shims could not be re-laid
/// comes back in the fourth slot and `run_repair` exits 1 on it, so a scripted
/// `repair && …` can tell fixed from refused.
#[cfg(unix)]
#[test]
fn a_shim_that_cannot_be_relaid_is_named_and_not_swallowed() {
    let l = temp_layout("repair-failed-lay");
    let d17 = seed_ready_build(&l, "ay", 17);
    activate_seeded(&l, "ay", &d17);
    let ay = crate::store::ToolName::new("ay").unwrap();
    // The seeded activation already laid `ay`, and a lay that finds its shim in place
    // writes nothing, so take it away; a read-only `bin/` is no obstacle (the lay
    // re-asserts the directory's mode), but a directory where the shim goes is: no rename
    // lands a file over it.
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

/// Repair puts back a program's own-name shim when only it is gone: with ty 3007 shipping
/// `bin/ty` and `bin/tla` and only `tla` (+`alab-tla`) left, repair must not derive ty's
/// exposes from the survivors alone. Control: an own-name shim present as a dev link into
/// a checkout keeps that link.
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

/// The lost-shim fixture: ty 3007, shipping `bin/ty` and `bin/tla`, laid with its `alab-`
/// aliases and activated, then `bin/ty` and `alab-ty` lost (with `all`, every shim of it).
/// Returns the layout and the build dir.
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

/// A tool a program ships under another name (`tla`, shipped by `ty`) answers for its lost
/// shim when the caller names the program (the reroute's question for `trustfmt`), and
/// only then: a standing shim, a build that does not ship it, or the wrong program answers
/// nothing.
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

/// An agent program's lost `bin/` shim is not "nothing runs" where its twin leads: the
/// `agents/` twin execs the store build, so with `agents/` ahead on PATH the managed copy
/// still runs and `which` names the twin; with no `agents/` on PATH nothing managed runs;
/// and a foreign copy ahead of `agents/` runs instead.
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

/// A lost shim is not an uninstalled program: with ty 3007 live and `bin/ty` gone, `run`
/// execs the store copy with a notice naming `aterm pkg repair`, `which` says the shim is
/// missing, and the named repair puts the shim back, after which the ordinary path answers
/// with no notice.
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
    }
}

/// A lost shim never revives what this machine turned off, and never answers for a name
/// the program does not own. Each control keeps the lost-shim shape and changes one thing;
/// every one answers None and never execs.
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

    // A tombstone among its other shims (the tla one), bin/ty absent.
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

/// An agent program with a lost shim is named, not run: its run hands `claude update` to
/// the package manager only through a shim that resolves, so exec'ing the store copy would
/// let the vendor's updater run. `run` says it is installed and names the repair; `which`
/// offers no `now: aterm pkg run`.
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

/// Repair keeps what the build exports: `relay_shims` re-lays each shim with its build's
/// sidecar environment, so `bin/claude` keeps `export DISABLE_AUTOUPDATER=1` and the
/// vendor's own updater stays off.
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

/// The pass heals a primary shim laid without its build's environment. An up-to-date
/// program is never laid again, so the alias reconcile re-lays such a primary first and
/// the `agents/` twin (and an ALab program's `alab-` alias) carry the healed environment in
/// the same pass; a shim exporting something else is put to exactly the declaration; a
/// second pass writes nothing; and a build with no sidecar keeps whatever its shim exports
/// (add-only).
#[cfg(unix)]
#[test]
fn the_pass_re_lays_a_primary_that_lost_its_builds_environment() {
    let l = temp_layout("pass-shim-env");
    let admit = |e: &str| crate::shim_env::ShimEnv::admit(&[e.to_string()]).unwrap();
    let env = admit("DISABLE_AUTOUPDATER=1");
    let dir = seed_ready_build(&l, "claude", 2_026_092_201);
    activate_seeded(&l, "claude", &dir); // plain: the drifted shape
    crate::shim_env::write_sidecar(&dir, &env).expect("sidecar");
    let claude = crate::store::ToolName::new("claude").unwrap();
    let (shim, twin) = (l.shim(&claude), l.agent_shim(&claude));
    // An ALab program whose shim exports other than its build declares, with its alias.
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

    // Add-only: no sidecar ⇒ the shim's own exports stand.
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

/// `repair`'s verdict counts an item it could not read (`Unknown`) as left undone, as it
/// counts a failed or blocked one: exit 1 and "not done", never "done".
#[test]
fn repair_counts_an_unread_item_undone() {
    let row = |outcome| aterm_converge::ItemReport {
        id: crate::converge::repair::EXEC_ROOTS.to_owned(),
        family: "pkg".to_owned(),
        title: "the trust build's exec root".to_owned(),
        outcome,
        from: None,
        to: None,
        why: Some("trust build 8595's exec root could not be read".to_owned()),
        revert: None,
        effect: aterm_converge::Effect::Now,
        detail: None,
        layer: Some(aterm_converge::Layer::Base),
        class: Some(aterm_converge::Class::Relay),
        explain: None,
        stood_aside: None,
        essential: false,
    };
    use aterm_converge::Outcome;
    for (outcome, undone) in [
        (Outcome::Unknown, true),
        (Outcome::Failed, true),
        (Outcome::Blocked, true),
        (Outcome::Ok, false),
        (Outcome::Changed, false),
        (Outcome::NotApplicable, false),
    ] {
        let rows = [row(outcome)];
        let want = if undone {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
        assert_eq!(super::repair_exit(&rows), want, "{outcome:?}");
        let verdict = super::repair_lines(&rows).pop().unwrap_or_default();
        assert_eq!(
            verdict.contains("not done"),
            undone,
            "{outcome:?}: {verdict}"
        );
    }
}

/// An rc `repair` leaves alone is said, with why: here a `~/.zshrc` that is a dangling
/// link, which no pass edits.
#[test]
fn repair_says_an_rc_it_left_alone() {
    let layout = temp_layout("repair-rc-alone");
    let home = crate::testdir::dir("atpkg-main-repair-rc-alone-home-");
    std::os::unix::fs::symlink(home.join("gone"), home.join(".zshrc")).unwrap();
    let ground = std::sync::Arc::new(crate::converge::repair::Ground {
        layout: crate::store::Layout {
            prefix: layout.prefix.clone(),
        },
        home: Some(home.to_path_buf()),
        exe: None,
        rustup_home: None,
        login_rc: None,
        pass: crate::converge::repair::Pass::Repair,
    });
    let rows = super::repair_rows(
        &ground,
        &mut aterm_converge::NoJournal,
        Some(&[crate::converge::repair::HOOKS, crate::converge::repair::RC]),
        aterm_converge::Scope::REPAIR,
    );
    let lines = super::repair_lines(&rows);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("repair: ~/.zshrc left alone — atpkg edits only")),
        "{lines:?}"
    );
}

/// Repair respects a rollback: `rollback` keeps the build it rolled off until gc, so repair
/// re-lays the build `current` selects, not the highest on disk, or it would undo the
/// rollback and mint the `LinkShimMismatch` doctor reports.
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

/// A complete trust build in the shape bundles 8571/8589/8590/8595 ship: `bin/rustc` a
/// separate file beside `bin/trustc` whose bytes differ (the ad-hoc signature bakes each
/// file's name in), a tippy that refuses that pair, and a `lib/` with a driver dylib, with
/// its ready marker, `store/trust/current` pointing at it, and plain shims for `trustc`,
/// `targo`, `tippy` and `alab-tippy` as a client before exec roots laid them. Returns the
/// build dir.
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
/// alias included, and answers `true`.
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
}

/// REPAIR ROUTES, AND SAYS SO IN ITS EXIT CODE WHEN IT CANNOT. An empty store still
/// reconciles roots left by a partial removal, and a failure there also exits 1.
/// Over an affected build
/// with plain shims, [`repair_store`] re-lays them through the root it lays and exits
/// 0; with the root unlayable (an unreadable directory in the build's `lib/`, standing
/// in for the EXDEV of a store on another volume) every shim is still re-laid — plain,
/// running the store path as before — and repair exits 1, because doctor's fix for a
/// PATH tippy that cannot run is exactly this verb.
#[test]
fn repair_routes_the_trust_shims_and_exits_1_when_the_root_cannot_be_laid() {
    let empty = temp_layout("exec-root-repair-empty");
    for reconciled in [false, true] {
        assert_eq!(
            repair_store_reconciling(&empty, &[], |_, _| reconciled),
            if reconciled {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            },
            "an empty store must retain its exec-root operation's verdict"
        );
    }

    let l = temp_layout("exec-root-repair");
    seed_affected_trust(&l, 8595);
    assert_eq!(repair_store(&l, &[]), ExitCode::SUCCESS);
    for s in ["trustc", "targo", "tippy", "alab-tippy"] {
        assert!(routes(&l, s), "{s} routes after repair");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);

    aterm_uds::host::could_not_run_as_root("a mode-0 directory");
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

/// A row does not outlive its program: `active_builds` reads shims, but Settings ▸
/// Packages reads the record. The stale-row reconciler rightly declines (the signed index
/// still names the program), so the uninstall retires the row.
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

    // The real path the CLI arm runs, not the primitive it calls.
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

/// A held-update record ends with the last build it moves off: doctor reports an outlived
/// record and changes nothing, so the doors that write end it (the uninstall that took its
/// builds, and `repair`, which doctor names for a store emptied by hand). Negative
/// control: a record one of whose builds still stands is kept by both.
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

/// Row retirement accepts only program names: `ops::uninstall`'s shape rule rejects only
/// empty/`.`/`..`/separators/NUL, so without the gate `atpkg uninstall '*toolset*'` would
/// delete a bookkeeping row with its own owner, and `atpkg uninstall --help` would mint a
/// removed-marker for a flag.
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
    // The removal intent is recorded before the delete, so the shape rule `ops::uninstall`
    // enforces must refuse first too: a path-shaped name is not a program to remember.
    assert!(uninstall_and_retire(&layout, "a/b").is_err());
    assert!(
        removed_programs(&layout).is_empty(),
        "no removed-marker is minted for a name the delete would refuse"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The removal intent lands before the delete. `ops::uninstall` takes the shims off and
/// then spends seconds in `remove_dir_all` (3.4 GB for trust); a kill inside the delete
/// must not leave the program absent and recorded nowhere, for the next pass to reinstall.
/// A subtree the delete cannot empty stops `remove_dir_all` where a kill would.
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
        aterm_uds::host::could_not_run(
            "a write into a mode-0500 folder succeeded: root, or a file system that ignores modes",
        );
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

/// An offline first run still leaves a witness: the environmental failures' reason goes
/// only to the outcome sentence, so `record_outcome` creates the record on a virgin store.
/// That failed-seed machine is the case doctor's fallback to this sentence exists for.
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

/// The record is a derived memo; the signed index is the authority on what exists. The
/// conditions are pinned together because each alone is a different bug: prune a sentinel
/// and the bookkeeping rows lose their reapers; prune a live-but-dropped program and
/// `atpkg verify` fails closed forever on an erased attestation.
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

/// Fail-closed: an account that cannot be resolved is not proof that HOME is it.
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

/// The same directory under another spelling is the real home; a different one, or a
/// missing one, is not.
#[cfg(unix)]
#[test]
fn the_same_directory_under_another_spelling_is_the_real_home() {
    // Unique per run (`crate::testdir`), so two runs of this suite never race on one
    // directory and symlink name.
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

/// A config that does not parse changes nothing: the `[machine]` default acts, so
/// "malformed ⇒ defaults" would let a typo three tables away re-enable what the owner
/// switched off. The refusal comes before the home question: it is cheaper, and the one
/// the owner can fix.
#[test]
fn an_unreadable_config_refuses_before_anything_is_measured() {
    let unreadable = crate::config::MachineConfig {
        unreadable: true,
        ..crate::config::MachineConfig::default()
    };
    assert!(
        machine_refusal(&unreadable).is_some(),
        "an unreadable config refuses"
    );
    // And it refuses even though the resolved value looks like the default.
    assert_eq!(
        unreadable.universal_control(),
        crate::config::UniversalControlPolicy::Off
    );
}

/// `machine`'s grammar is three spellings and nothing else: bare (read), `apply` and
/// `cache` (the build cache housekeeping's dry run). Anything more is a usage error, so a
/// typo never silently reads when someone meant to apply.
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
}

/// `--help` answers, never acts, for every verb.
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

/// A `--help` that belongs to the tool is forwarded, not answered: `run` looks only at
/// its first operand, and a literal `--` ends the scan for every verb.
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

/// `lane` at the dispatch edge: a stock command line is refused (exit 2), a Trust one is
/// not (0), and a `--help` after `--` belongs to the command, so `lane -- cargo --help` is
/// read, not answered with lane's own usage.
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

/// A provisioned Mac's launch never touches the network for its seedless sentence:
/// `report_seedless_posture` resolves the signed index (a metered listing, verification,
/// a floor record, under the store lock) only for an empty store a typed `aterm pkg seed`
/// asks about, where its verdict is the point; a store that holds builds prints the same
/// line either way. The window's own seed (`--wait-lock`) never resolves, since the update
/// right behind it says the same verdict (`a_window_seed_asks_no_index_and_a_typed_one_does`
/// in `tests/seed_disabled.rs` drives both at the process edge).
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
}

/// The seed never calls a machine empty-handed while a vendor-direct program is still to
/// land: agy and muse are in no index, so an `exclude` naming every index program and
/// claude and codex leaves `seed_serviceable` empty while the pass right behind the seed
/// installs both.
#[test]
fn a_wanted_vendor_program_serves_the_seed_without_the_index() {
    let darwin = "aarch64-apple-darwin";
    let none = std::collections::BTreeSet::new();
    let excluding = |names: Vec<String>| crate::config::PackagesConfig {
        exclude: Some(names),
        ..Default::default()
    };
    let programs = || {
        crate::vendor_direct::VENDORS
            .iter()
            .map(|s| s.program.to_string())
    };
    assert!(vendor_set_wanted_on(
        darwin,
        &crate::config::PackagesConfig::default(),
        &none
    ));
    // Every vendor program excluded: the index alone answers.
    assert!(!vendor_set_wanted_on(
        darwin,
        &excluding(programs().collect()),
        &none
    ));
    // One left still serves; an uninstall takes it out as an exclude does.
    let all_but_muse = excluding(programs().filter(|p| p != "muse").collect());
    assert!(vendor_set_wanted_on(darwin, &all_but_muse, &none));
    assert!(!vendor_set_wanted_on(
        darwin,
        &all_but_muse,
        &std::collections::BTreeSet::from(["muse".to_string()])
    ));
    // Off darwin, Meta's binary is unserved: wanted there, it promises nothing.
    assert!(!vendor_set_wanted_on(
        "x86_64-unknown-linux-gnu",
        &all_but_muse,
        &none
    ));
}

/// Adoption keeps a user's toolchain whole as the suite grows (§11): under the one install
/// consent (`auto_install`, default on) a machine that runs the set gets newly published
/// members without being asked again. A lifecycle error is silent both ways: a missing
/// marker decays the toolset, a sticky one reinstalls what the user removed.
#[test]
fn adoption_is_recorded_by_the_whole_set_and_forgotten_by_uninstall() {
    let dir = crate::testdir::dir("atpkg-adopt-");
    let layout = crate::store::Layout {
        prefix: dir.clone(),
    };

    // A fresh store has not adopted.
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

    // Adoption is the file's existence, never a parsed value, so a garbled or truncated
    // write cannot read as another answer.
    std::fs::write(layout.adopted(), b"\0\0garbage").unwrap();
    assert!(adopted(&layout), "existence is the whole predicate");

    // Uninstall forgets it: set-completion must never undo an explicit removal.
    clear_adoption(&layout);
    assert!(!adopted(&layout), "uninstall opts out of set completion");
    // Clearing what is already clear is not an error.
    clear_adoption(&layout);

    // The whole-set verb's decision table: an install adopts, and so does a set found
    // already complete; a machine with no builds before or after, a failed member, a
    // skipped member or an owed member does not.
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

/// A targeted removal sticks too: without a per-program record the resumable seed lane
/// would bring `atpkg uninstall ny` back on the next launch. Asking for it back lifts the
/// record.
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

/// A removal that could not be recorded is said. An unwritable prefix (a read-only store,
/// a full disk, a `removed` path something else owns) must not leave `uninstall` printing
/// success with nothing recorded: this file alone keeps the program out of the unattended
/// lanes (`seed_serviceable` and `install_default_set_inner` filter on
/// `removed_programs`). `write_removed` returns the failure, the recorder prints it, and
/// the entries already recorded survive the temp+rename.
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
    let _ = std::fs::remove_dir_all(&dir);
}

/// Removal must stick: the resumable seed lane installs whatever the store lacks, so it
/// would reinstall everything `uninstall --all` removed on the next launch. The decline
/// marker stops that and an explicit install lifts it; both directions are pinned.
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

    // Decline and adoption are independent facts: declining must not read as adoption,
    // or a removed toolset would still get set completion.
    record_decline(&layout);
    assert!(!adopted(&layout), "declining is not adopting");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The marker contract has a positive terminal (`seed-done:`), and the ledger knows which
/// half of the contract each marker keeps. Without it a pass that announced itself, ended
/// well and had no roster to name printed nothing, and the GUI read that silence as a
/// failure.
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
    // `lock-waiting:`, which is NEITHER half, stays out of the table: a
    // `lock-waiting:` line answers nothing and opens nothing, so the ledger
    // must read straight past it (a child that waited and then announced is
    // an OPEN announcement, not an answered one).
    assert!(
        !MARKERS.iter().any(|(m, _)| *m == LOCK_WAITING_MARKER),
        "`lock-waiting:` is neither an announcement nor an answer"
    );
    // …and so does `lock-acquired:`, which answers the wait, not an announcement: were it
    // an answer, a child that waited, acquired and then announced would look answered
    // before it began.
    assert!(
        !MARKERS.iter().any(|(m, _)| *m == LOCK_ACQUIRED_MARKER),
        "`lock-acquired:` is neither an announcement nor an answer"
    );
    use super::announcement;
    let _ledger = announcement::own_ledger();
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
}

/// The ledger reads the same bytes the GUI parses, and an informational row is not an
/// answer: `managed-current:` / `machine-settings:` print at the end of every pass,
/// announced or not, and counting one as a terminal would let a pass retire a card it had
/// not finished with.
#[test]
fn only_a_terminal_marker_closes_an_announcement() {
    use super::announcement;
    let _ledger = announcement::own_ledger();
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
    // THE PROPERTY (2026-08-20 audit): a pass that announced nothing answers
    // nothing — `answer_announcement` runs at every announcing verb's tail, and with no
    // announcement open it is a no-op for BOTH verdicts, or the warning card is back on
    // screen at every launch of a healthy machine.
    for ok in [true, false] {
        answer_announcement(ok);
        assert!(!announcement::is_open(), "nothing to answer (ok={ok})");
    }
}

/// The wire lane's announcement, as the window reads it: the `net-starting:` prefix and
/// the set, then the signed sizes as the line's one trailing parenthetical (the toolchain
/// bar's input), within the bar's and the log's bounds — and no line at all for a pass
/// with nothing to install.
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
            "atpkg: {NET_STARTING_MARKER}installing {} program(s) over the network: {} — ",
            set.len(),
            set.join(", ")
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
    // No signed sum ⇒ no size at all (no number beats a wrong one), the
    // switches still named, and the last '(' is then "program(s)", whose
    // tail does not close — the bar shows "starting…" rather than a guess.
    let bare = net_announcement(Consent::InstallingAterm, &set, SignedSizes::UNKNOWN)
        .expect("a non-empty network pass announces before acting");
    assert!(!bare.contains("GiB") && !bare.contains("GB"), "{bare}");
    assert!(bare.contains(NET_OFF_SWITCH), "{bare}");
    let (_, tail) = bare.rsplit_once('(').expect("program(s)");
    assert!(!tail.ends_with(')'), "{bare}");
    // …and the ordinary no-op tick stays silent, whoever consented.
    for consent in [Consent::InstallingAterm, Consent::Explicit] {
        assert!(net_announcement(consent, &[], both).is_none());
    }
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
}

/// The consent policy as a table ([`should_complete_set`] states each input's reason).
#[test]
fn set_completion_honours_every_opt_out_over_the_one_consent() {
    // (consent, adopted, asked, enabled, declined, unreadable)
    // The batteries-included default on an adopted machine: keep the set complete.
    assert!(should_complete_set(true, true, false, true, false, false));
    // Not adopted — a fresh store before its seed pass, or a seed refusal that took
    // its adoption back for want of disk — installs nothing new.
    assert!(!should_complete_set(true, false, false, true, false, false));
    // …unless `auto_install = true` is written: the one way a machine no window runs on
    // gets the set (and gets it back after `uninstall <program>` cleared adoption).
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
    // Declined outranks the consent, a written one too.
    assert!(
        !should_complete_set(true, true, true, true, true, false),
        "declining outranks the install consent — it is the later, more specific act"
    );
    // An unreadable [packages] table outranks it too: it has no `exclude` to narrow the
    // lane with.
    assert!(
        !should_complete_set(true, true, true, true, false, true),
        "the set waits for the config to be readable before it is completed from an \
         exclude list nobody could read"
    );
}

/// The work rule the next-index probe asks before it spends a HEAD: an empty store that is
/// not completing the set has nothing an index could move, so its `update` answers
/// `EMPTY_UPDATE` and no published index wakes one. Controls: an installed program, set
/// completion, or a development link is work.
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

/// The probe worker outlives a CLI invocation: an adopted but empty store starts looking
/// for newly published members once the user enables Automatic updates in the same running
/// window or session, without a restart.
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

/// The two durable ratchets advance together and independently: an index build and a
/// roster generation are different counters over different documents, and one shared
/// high-water would let either one's advance refuse the other's current document.
#[test]
fn advance_floors_ratchets_both_counters_separately() {
    let layout = temp_layout("floors");
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
    advance_floors(&layout, 41, 3);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 41);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
    // An index republish that does not bump the roster leaves the roster floor put, so
    // the generation still in use is not ratcheted out from under the client.
    advance_floors(&layout, 42, 3);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 3);
    // A revocation bumps the roster without re-cutting the index — symmetrically.
    advance_floors(&layout, 42, 4);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 42);
    assert_eq!(crate::sig::Floor::new(layout.roster_floor()).current(), 4);
    // And the anchor the CLI builds carries the roster floor it just recorded.
    assert_eq!(effective_anchor(&layout).roster_floor, 4);
    // Non-vacuity: they are separate files.
    assert_ne!(layout.floor(), layout.roster_floor());
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A build floor one machine drove out of reach is re-based by the next generation:
/// `index_build` sits inside machine-signed bytes, so a rostered machine could set it to
/// the u64 ceiling and lock the store above every index the owner publishes, including
/// the one revoking it. A strictly newer master-signed generation re-bases the floor;
/// only the paper master mints a generation, so the lever is the master's alone.
/// Mutation: replacing the `roster_seq > generation.current()` arm in `advance_floors` with
/// the plain `check_and_record` fails the rescue assertion.
#[test]
fn a_newer_generation_rebases_a_floor_a_machine_drove_out_of_reach() {
    let layout = temp_layout("floor-poison");
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();

    // A machine on generation 3 publishes the TOML integer ceiling.
    advance_floors(&layout, u64::MAX, 3);
    assert_eq!(build_floor(&layout).index_build, u64::MAX);
    assert_eq!(build_floor(&layout).roster_seq, 3);
    // Precondition: within that same generation the floor is immovable, so this really
    // is the trap.
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
// Signed-fixture helpers mirroring flow.rs's (a real USTAR+zstd archive, a machine-signed
// index beside the master-signed roster, signed pkg manifests) laid out as a dir registry
// so the production `DirFetcher` serves them (§14).

use ring::signature::{Ed25519KeyPair, KeyPair};
use std::path::{Path, PathBuf};

use crate::sig::testkit;

/// The synthetic paper master and the one machine it rosters: the fixture the whole crate
/// signs with, so a CLI test proves nothing under a trust shape no other layer uses.
const ROOT_SEED: [u8; 32] = testkit::MASTER_SEED;
const RELEASE_SEED: [u8; 32] = testkit::MACHINE_SEED;

/// The anchor the dir-registry tests resolve under: armed with the synthetic master,
/// roster floor 0.
fn test_anchor() -> crate::Anchor {
    crate::Anchor::of(vec![pk(&ROOT_SEED)], 0)
}

/// Publish the master-signed roster beside a `dir:` registry's index. Without it the
/// DirFetcher yields no candidate: a registry is the index plus the generation that
/// authorized its signer.
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
    std::fs::write(&path, crate::extract::fixtures::zstd_bytes(&tar)).unwrap();
    path
}

/// Write one program's release-signed `pkg-<prog>-<build>.toml` (+ `.sig`) beside its
/// real archive, with the archive's genuine sha256 + tree_root baked in.
fn write_pkg(dir: &Path, prog: &str, build: u64) {
    write_pkg_for_triple(dir, prog, build, current_triple());
}

/// [`write_pkg`] with an explicit artifact `target`: a manifest whose one artifact serves
/// a foreign triple is how the registry looks from every machine the publisher does not
/// build for.
fn write_pkg_for_triple(dir: &Path, prog: &str, build: u64, triple: &str) {
    write_pkg_exposing(dir, prog, build, triple, &[prog]);
}

/// [`write_pkg_for_triple`] whose manifest exposes `exposes` (its archive holds `bin/<prog>`).
fn write_pkg_exposing(dir: &Path, prog: &str, build: u64, triple: &str, exposes: &[&str]) {
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
        crate::extract::FrameChecksum::IfPresent,
    )
    .unwrap()
    .root();
    let _ = std::fs::remove_dir_all(&probe);
    let exposes = exposes
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        "schema = 2\nprogram = \"{prog}\"\nversion = \"0.1\"\nbuild_number = {build}\n\
         exposes = [{exposes}]\n\
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

/// Write the machine-signed index for a 4-program default set on `channel`: `ay`
/// (installable), `ny` (a second installable, for exclude), `zz` (pin yanked: never
/// installs), `lk` (installable, dev-linked in the tests), plus signed pkgs and archives
/// for the non-yanked members.
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
    // The index is machine-signed; only the roster is signed by the master.
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

/// A counting [`crate::DirFetcher`]: every index resolve reaches the fetcher through
/// `index_candidates`, so the counter is the instrument for "the pass verify-selects once".
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
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
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
        // Not 0, the "clock unusable" sentinel that stamps nothing (`rfc3339_at`).
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
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
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

/// An empty store whose first index resolve was refused, or met a link that was down,
/// installs on the set lane's retry and records the pass as the success it was.
#[test]
fn empty_store_retry_after_refusal_reports_success() {
    for first_index_link_down in [false, true] {
        assert_empty_store_retry_success(first_index_link_down);
    }
}

/// A Rosetta pass writes its own row: the translated refusal (exit 2) writes its verdict
/// over an earlier pass's "index unreachable" `*toolset*` row, which the window would
/// otherwise read as this pass's.
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
        // Not 0, the "clock unusable" sentinel that stamps nothing (`rfc3339_at`).
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
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
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

/// The update says the seed's verdict, line for line. The window's own seed asks no index
/// (`report_seedless_posture`, a `--wait-lock` caller), so the update right behind it,
/// which resolves the index anyway, is where an unserved architecture or a toolset
/// narrowed to nothing reaches the window's card: its empty-store arm prints the line the
/// seed prints for that verdict, beside its stderr sentence and its exit 2. A pass that
/// never resolved an index has no verdict about the machine and prints none (the negative
/// control: offline is not "unserved").
#[test]
fn the_updates_empty_store_verdict_prints_the_seeds_unusable_line() {
    let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let cfg = crate::config::PackagesConfig {
        exclude: Some(names(&["ay", "ny", "zz", "lk", "claude", "codex"])),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let seams = PassSeams {
        anchor: |_| test_anchor(),
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
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
    };
    let narrowed = Narrowing {
        excluded: names(&["ay", "lk", "ny", "zz"]),
        removed: Vec::new(),
    }
    .verdict(current_triple())
    .0;
    let unserved = unserved_sentence(current_triple(), &names(&["riscv64gc-unknown-linux-gnu"]));
    for (case, published_for, expected) in [
        ("narrowed", Some(current_triple()), Some(narrowed)),
        (
            "unserved",
            Some("riscv64gc-unknown-linux-gnu"),
            Some(unserved),
        ),
        ("unreachable", None, None),
    ] {
        let dir = scratch(&format!("update-unusable-{case}"));
        if let Some(triple) = published_for {
            write_registry_at(&dir, "stable", 46);
            for (prog, build) in [("ay", 18), ("ny", 7), ("lk", 3), ("zz", 5)] {
                write_pkg_for_triple(&dir, prog, build, triple);
            }
        }
        let layout = temp_layout(&format!("update-unusable-{case}"));
        let _ = take_markers_said();
        let code = run_recorded_update_pass(
            &layout,
            &cfg,
            &crate::DirFetcher::new(dir.clone()),
            &std::collections::BTreeMap::new(),
            true,
            &seams,
        );
        let unusable: Vec<String> = take_markers_said()
            .into_iter()
            .filter(|line| line.contains(SEED_UNUSABLE_MARKER))
            .collect();
        if expected.is_some() {
            assert_eq!(code, 2, "{case}: the empty store's verdict exits 2");
        }
        assert_eq!(
            unusable,
            expected
                .iter()
                .map(|why| seed_unusable_line(why))
                .collect::<Vec<_>>(),
            "{case}"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// One pass, one verify-select: `apply_channel` resolves and verifies the signed index,
/// and every other lane of the same pass (set completion, the attestation repair, the
/// alias reconcile, the empty-store diagnosis) takes that index rather than re-reading the
/// cache, re-checking every signature and re-admitting the roster for an answer that
/// cannot differ inside one pass. The attestation repair reads the real clock, so a
/// fixture index whose signed window is long past cannot exercise it here.
#[test]
fn a_pass_verify_selects_the_index_once_and_hands_it_to_every_lane() {
    let dir = scratch("one-resolve");
    write_registry(&dir, "stable");
    let layout = temp_layout("one-resolve");
    let fetcher = CountingFetcher::new(dir.clone());
    // `ny` and `zz` are narrowed out so both measured passes run over a complete set: a
    // lane with members to install also runs the per-program `flow::install` path, which
    // resolves for itself and would drown the count.
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

    // On the shape the six-hourly tick meets (a complete set, nothing to install), the
    // set-completion lane handed the pass's index verify-selects nothing.
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
        super::SetCompanions::alone(),
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

    // …while the same pass with no index in hand resolves one for itself.
    let out = install_default_set(&layout, &fetcher, &test_anchor(), &cfg, 0);
    assert_eq!(out.index_error, None);
    assert_eq!(
        fetcher.resolves(),
        settled + 1,
        "a lane holding no index resolves one"
    );
    let settled = fetcher.resolves();

    // No gate is skipped: past the signed `valid_until` the held index is refused re-use,
    // and the lane's own resolve meets the same freshness refusal.
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
        super::SetCompanions::alone(),
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
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&co);
}

/// An update pass separates a present local toolset from a network check: a dev-link-only
/// pass succeeds without inventing a registry build or check, and a checkout gone before
/// the final inventory still fails.
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

// The auto-install decision matrix (§11), against the production DirFetcher over a real
// signed dir registry: a missing default-set member installs, an `exclude`d member is
// narrowed out, a dev-linked member hard-skips, a yanked pin never installs, and an
// already-installed member is left to the update pass. No skip counts as a failure.
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
// 5 and `yanked = ["zz@5"]`: the default-set pass skips
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
    // The default-set lane: skipped, said, recorded. (The pure gate's verdict is
    // `gate.rs`'s `pin_below_floor_or_yanked_tombstones`.)
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
    // The update lane over a live copy of the yanked build: tombstoned with the same
    // words, shims off PATH.
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

/// An empty store's verdict names its real cause: `seed_serviceable` is empty both when
/// the index publishes nothing for this triple and when the user's `[packages].exclude` or
/// uninstalls took everything it does publish. `index_serves` asks again without the
/// narrowing, and only an empty unnarrowed set blames the CPU.
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
    let (_, state) = narrowing.verdict(current_triple());
    assert_eq!(state, crate::state::TOOLSET_REMOVED);
    assert!(!crate::doctor::is_recorded_problem("*toolset*", state));
    // Both at once: each member named under its own cause, and the exclusion's
    // state — a failure a person can act on — is the one recorded.
    std::fs::write(layout.removed(), "ny\nlk\nzz\n").unwrap();
    let mixed = index_serves(&layout, &fetcher, &index, &exclude(&["ay"]));
    let IndexServes::Narrowed(narrowing) = mixed else {
        panic!("{mixed:?}")
    };
    assert_eq!(narrowing.excluded, names(&["ay"]));
    assert_eq!(narrowing.removed, names(&["lk", "ny", "zz"]));
    let (_, state) = narrowing.verdict(current_triple());
    assert_eq!(state, crate::state::TOOLSET_EXCLUDED);
    assert!(crate::doctor::is_recorded_problem("*toolset*", state));
    // Negative control: when nothing is published for this triple the architecture is
    // the answer, exclusions and removals notwithstanding.
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

/// A program the registry publishes no artifact for on this triple neither announces, nor
/// counts as a failure, nor leaves a pending stub, and its own row records `unavailable on
/// <target>: <hint>`, so a pre-existing stub's `__pending` tells the truth (design §2).
/// Served members still install, and because one does, the toolset-wide `blocked` verdict
/// (for a machine the index serves nothing on) is not raised.
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
    // (`zz`'s yanked pin is a fault: its `tombstoned:` row is the point of
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

/// The retired `requires` through the set-completion pass (§5.3(b)/(c)): `ay`'s signed row
/// names it, so the pass refuses ay (an `error:` row naming the refusal, nothing
/// downloaded, no artifact resolved) while `ny` and the former extra `xc` install beside it
/// as default-set members.
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

// An UpToDate pass rewrites every live member's row to name this pass's index (migrating
// the legacy `active` spelling and re-stamping `pinned by index 15`) while keeping the
// pass's own outcome sentence, and leaves every other spelling (a SHADOWED row, a fault)
// to the arm that owns it.
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
    // A fault row a failed pass wrote is lifted: a member this pass finds live at its pin
    // has, by that fact, no such fault.
    for fault in [
        "aborted: stage",
        "aborted: flip (rolled back)",
        "error: coherence group 'trust' bootstrap aborted",
        // An older client's requirement-gate row (§5.3(b)).
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
    // A live build cannot inherit the recorded attestation of an older build,
    // even while this same reporting pass migrates the row's state spelling.
    record_status(
        &layout,
        "ay",
        crate::ProgramStatus {
            installed_build: Some(17),
            state: "active".into(),
            tree_root: "signed-for-17".into(),
        },
        "previous pass".into(),
    );
    let (_, failures) = update_pass(&layout, &fetcher);
    assert_eq!(failures, 0);
    let status = crate::status::read(&layout).unwrap();
    assert_eq!(status.programs["ay"].installed_build, Some(18));
    assert!(
        status.programs["ay"].tree_root.is_empty(),
        "a root never migrates between builds"
    );
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
/// installed member's row says which build is unpublished and which one stays, and a
/// channel stepping back lifts the held rows. (That nothing moves or stages is
/// `flow_tests`' `an_installed_group_whose_new_pin_lacks_this_triple_is_held_whole`; a
/// pin published here upgrades: the control below.)
#[test]
fn an_installed_group_whose_new_pin_is_unpublished_here_is_held_not_failed() {
    let dir = scratch("update-held-triple");
    write_installed_group_registry(&dir, "riscv64gc-unknown-linux-gnu", &[]);
    let layout = temp_layout("update-held-triple");
    let fetcher = crate::DirFetcher::new(dir.clone());
    make_installed_group_live(&layout);

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
    let triple = current_triple();
    let status = crate::status::read(&layout).expect("recorded");
    assert_eq!(
        status.programs["tb"].state,
        crate::state::held_unpublished(None, 6, triple, 5)
    );
    assert_eq!(
        status.programs["ta"].state,
        crate::state::held_unpublished(Some("tb"), 6, triple, 3)
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
    let triple = current_triple();
    // A live shim is an `exec` stub too (a regular file), so what marks the tombstone is
    // that it forwards nowhere and says why, and that the store stops counting the revoked
    // build as live.
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
    // …and so does the set-completion prescan that runs next in the same pass on an
    // adopted machine: ta is neither installed nor published here, so the prescan counts
    // it unserved, but must not repaint its fault row as the benign `unavailable on
    // <triple>: …`. Its measurement is `program_disabled_here`: the recorded row, or a
    // tombstone shim.
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

// THE ONE CHANNEL: the bootstrap resolves `stable` ([`crate::config::CHANNEL`]) —
// a registry publishing only `nightly` installs nothing, loudly, and a retired
// `[packages].channel = "nightly"` in the file changes nothing (2026-09-23). The
// `stable` control is `default_set_bootstrap_installs_missing_and_skips_excluded_linked_yanked`.
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
    let _ = std::fs::remove_dir_all(&dir);
}

// An unresolved index is not a check: an adopted machine with an empty store runs `update`
// through the set-completion lane alone, and offline (or on a channel the index does not
// publish) that lane fails before it checks anything. It stamps no `last_success_at`,
// records the apply lane's `*index*` row, and a resolved pass retires the row.
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

/// Write a machine-signed index whose default set is the `rustc`-locked coherence tuple
/// `ta`+`tb` plus the ungrouped singleton `ay` (the §7 bootstrap fixture).
/// `write_tb_pkg = false` leaves tb's pkg manifest unpublished, so its stage fails and the
/// group transaction must abort whole.
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
    // The index is machine-signed; only the roster is signed by the master.
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

// §7 all-or-nothing: a tuple member whose pkg manifest is unpublished aborts the whole
// group. The sibling that staged fine is neither activated nor left on disk (a
// complete-but-inactive build would read as active next run), the ungrouped singleton
// still installs, and the group counts one failure.
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

/// A [`crate::DirFetcher`] that records, in order, every manifest pair it was asked to
/// prefetch (`true`) and every one it was then asked to read (`false`).
struct PrefetchRecorder {
    inner: crate::DirFetcher,
    asked: std::cell::RefCell<Vec<(bool, String, u64)>>,
}
impl crate::flow::Fetcher for PrefetchRecorder {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        crate::flow::Fetcher::index_candidates(&self.inner)
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.asked
            .borrow_mut()
            .push((false, program.to_string(), build));
        crate::flow::Fetcher::pkg_manifest(&self.inner, repo, program, build)
    }
    fn prefetch_manifests(&self, pins: &[(String, String, u64)]) {
        let mut asked = self.asked.borrow_mut();
        for (_, program, build) in pins {
            asked.push((true, program.clone(), *build));
        }
    }
    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        crate::flow::Fetcher::download(&self.inner, repo, asset, dest)
    }
    fn source_id(&self) -> String {
        crate::flow::Fetcher::source_id(&self.inner)
    }
}

// The set's manifests at once: before the set completion reads its first manifest, one
// prefetch has asked for every pinned manifest its missing-triple prescan reads (one at a
// time, they are a GET apiece ahead of the announcement), and every read the pass then
// makes is of a pair it asked for.
#[test]
fn the_set_completion_asks_its_manifests_at_once_before_reading_one() {
    let dir = scratch("prefetch-prescan");
    write_registry(&dir, "stable");
    let layout = temp_layout("prefetch-prescan");
    let fetcher = PrefetchRecorder {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::cell::RefCell::new(Vec::new()),
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert_eq!(out.index_error, None);
    assert_eq!(
        crate::active_builds(&layout).get("ay").copied(),
        Some(18),
        "a real pass: the missing members installed"
    );
    let asked = fetcher.asked.into_inner();
    let first_read = asked
        .iter()
        .position(|(prefetch, ..)| !prefetch)
        .expect("the pass read manifests");
    let mut prefetched: Vec<(&str, u64)> = asked[..first_read]
        .iter()
        .map(|(_, program, build)| (program.as_str(), *build))
        .collect();
    prefetched.sort_unstable();
    assert_eq!(
        prefetched,
        [("ay", 18), ("lk", 3), ("ny", 7), ("zz", 5)],
        "every pin the prescan reads, asked before the first read: {asked:?}"
    );
    for (_, program, build) in asked[first_read..].iter().filter(|(p, ..)| !p) {
        assert!(
            prefetched.contains(&(program.as_str(), *build)),
            "{program}@{build} was read without being asked at once: {asked:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A [`crate::DirFetcher`] a pass may share, recording which thread asked for each archive;
/// with `refuse`, every download fails as a dropped link does.
struct SharedDownloads {
    inner: crate::DirFetcher,
    asked: std::sync::Mutex<Vec<(String, String)>>,
    refuse: bool,
}
impl crate::flow::Fetcher for SharedDownloads {
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
        let thread = std::thread::current().name().unwrap_or("").to_string();
        self.asked.lock().unwrap().push((asset.to_string(), thread));
        if self.refuse {
            return Err(String::from("curl: (6) Could not resolve host"));
        }
        crate::flow::Fetcher::download(&self.inner, repo, asset, dest)
    }
    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }
    fn source_id(&self) -> String {
        crate::flow::Fetcher::source_id(&self.inner)
    }
}

/// EVERY COMMAND A PROGRAM'S SIGNED MANIFEST EXPOSES ANSWERS BEFORE IT LANDS, not only the
/// branded names the compiled table knows: the set completion's reconcile lays a pending stub
/// for each, read from the manifest its probe verified, and the stub names its program
/// (`ay-maxsat` comes with `ay`), which is what `__pending` waits on. Here every download
/// fails, so ay stays absent and its stubs stand once the pass is over.
#[test]
fn a_command_only_the_manifest_exposes_gets_a_pending_stub_naming_its_program() {
    let dir = scratch("exposed-stub");
    write_registry(&dir, "stable");
    write_pkg_exposing(&dir, "ay", 18, current_triple(), &["ay", "ay-maxsat"]);
    let layout = temp_layout("exposed-stub");
    let fetcher = SharedDownloads {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::sync::Mutex::default(),
        refuse: true,
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert!(out.failures > 0, "every download is refused");
    assert!(!crate::active_builds(&layout).contains_key("ay"));
    assert!(
        crate::stub::pending_stub_exists(&layout, "ay-maxsat"),
        "the command the manifest exposes has its stub"
    );
    assert_eq!(crate::stub::program_of_in(&layout, "ay-maxsat"), "ay");
    assert!(crate::stub::pending_stub_exists(&layout, "ay"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A NAME ANOTHER PROGRAM OWNS, OR THE REROUTE TABLE ANSWERS, IS NO COMMAND STUB. `lk`'s
/// manifest also exposes `ay` — a program of the index's own, removed on this machine — and
/// `z3`, an upstream name the reroute table routes: no stub naming `lk` stands for `ay` (so
/// `aterm pkg install ay` asks for `ay` back rather than pointing at `lk`), none stands for
/// `z3`, and `lk`'s own command still gets its stub naming `lk`.
#[test]
fn a_manifest_exposing_another_programs_name_lays_no_stub_over_it() {
    let dir = scratch("exposed-owned");
    write_registry(&dir, "stable");
    write_pkg_exposing(&dir, "lk", 3, current_triple(), &["lk", "lk-x", "ay", "z3"]);
    let layout = temp_layout("exposed-owned");
    record_removed(&layout, "ay");
    let fetcher = SharedDownloads {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::sync::Mutex::default(),
        refuse: true,
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert!(out.failures > 0, "every download is refused");
    assert_eq!(crate::stub::program_of_in(&layout, "lk-x"), "lk");
    assert!(
        !crate::stub::pending_stub_exists(&layout, "ay"),
        "a removed program's name is no other program's command"
    );
    assert_eq!(crate::stub::program_of_in(&layout, "ay"), "ay");
    assert!(
        !layout
            .shim(&crate::store::ToolName::new("z3").unwrap())
            .exists(),
        "no stub for a rerouted upstream name"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// The set completion fetches ahead: with a fetcher it may share, every archive of the set
// (the tuple's members and the singleton after them) comes off the download-ahead lane's
// thread, once each, while the pass's own thread stages and flips.
#[test]
fn the_set_completion_takes_its_archives_from_the_download_ahead_lane() {
    let dir = scratch("set-ahead");
    write_group_registry(&dir, "stable", true);
    let layout = temp_layout("set-ahead");
    let fetcher = SharedDownloads {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::sync::Mutex::default(),
        refuse: false,
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert_eq!(out.failures, 0);
    let active = crate::active_builds(&layout);
    for (program, build) in [("ay", 18u64), ("ta", 4), ("tb", 6)] {
        assert_eq!(active.get(program).copied(), Some(build), "{program}");
    }
    let mut asked = fetcher.asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(
        asked
            .iter()
            .map(|(asset, thread)| (asset.as_str(), thread.as_str()))
            .collect::<Vec<_>>(),
        [
            ("ay-18.tar.zst", "atpkg-download-ahead"),
            ("ta-4.tar.zst", "atpkg-download-ahead"),
            ("tb-6.tar.zst", "atpkg-download-ahead"),
        ]
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// A set the disk check could not size starts no lane: tb's signed manifest does not
// arrive, so the set's bytes are unknown, and planning a lane would ask for that manifest
// once more (a connect timeout apiece on a dead link) before the first unit starts. The
// set lands one download at a time on the pass's own thread: the tuple refused, ay in.
#[test]
fn a_set_whose_size_is_unknown_lands_without_the_download_ahead_lane() {
    let dir = scratch("set-ahead-unsized");
    write_group_registry(&dir, "stable", false);
    let layout = temp_layout("set-ahead-unsized");
    let fetcher = SharedDownloads {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::sync::Mutex::default(),
        refuse: false,
    };
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert_eq!(out.failures, 1, "the tuple, whose tb cannot be fetched");
    assert_eq!(crate::active_builds(&layout).get("ay").copied(), Some(18));
    let asked = fetcher.asked.into_inner().unwrap();
    assert!(
        asked.iter().any(|(asset, _)| asset == "ay-18.tar.zst"),
        "{asked:?}"
    );
    assert!(
        asked
            .iter()
            .all(|(_, thread)| thread != "atpkg-download-ahead"),
        "{asked:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// Every archive lost is the pass's own fetch fault: offline, the lane's first download
// fails and the pass fetches the rest itself, so each failure the set completion counts is
// a fault on the pass's thread, the equation its offline verdict reads.
#[test]
fn a_set_whose_every_download_fails_counts_each_failure_as_a_fetch_fault() {
    let dir = scratch("set-ahead-offline");
    write_group_registry(&dir, "stable", true);
    let layout = temp_layout("set-ahead-offline");
    let fetcher = SharedDownloads {
        inner: crate::DirFetcher::new(dir.clone()),
        asked: std::sync::Mutex::default(),
        refuse: true,
    };
    let faults = crate::flow::fetch_faults();
    let out = install_default_set(
        &layout,
        &fetcher,
        &test_anchor(),
        &crate::config::PackagesConfig::default(),
        0,
    );
    assert_eq!(out.failures, 2, "the tuple and the singleton");
    assert_eq!(
        crate::flow::fetch_faults().wrapping_sub(faults),
        out.failures,
        "every failure a fetch fault: {:?}",
        fetcher.asked.lock().unwrap()
    );
    assert!(crate::active_builds(&layout).is_empty());
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

// §7 narrowing: an `exclude` that leaves part of a coherence tuple out refuses the whole
// group (a loud diagnostic, not a failure) while the ungrouped singleton still installs.
// The refusal is decided at planning: the refused members ride out on the outcome
// (`skipped_narrowed`), are not counted in `remaining_missing`, and keep no pending stub,
// which would promise the install the pass refused.
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

// A legacy `*seed*` offer row is swept row-only (the pass's own outcome sentence
// survives), by a clean set-completing pass and by the seed verb.
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

// A `[packages.links]` owner/repo override for a program the signed index does not name
// can never install anything; the bootstrap only says so loudly. Fail-closed: no unsigned
// installs from slugs.
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

// A hand-made dev-link pointing at a different checkout is never re-pointed by config:
// the link and its shim target survive untouched.
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

// `__pending`: the five states, the honesty edge, escape stripping (design §5).

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

/// [`pending_state`] collected non-interactively (no TTY, no aterm): the lines only.
fn pending_state_lines(layout: &crate::store::Layout, tool: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut say = |line: String| out.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: None,
        asker: crate::progress::Asker::Script,
    };
    pending_state(layout, tool, &mut io);
    out
}

/// [`pending_state`] for a person at a terminal who can wait: the lines and what the stub
/// does next.
fn pending_state_waiting(layout: &crate::store::Layout, tool: &str) -> (Vec<String>, PendingNext) {
    let mut out: Vec<String> = Vec::new();
    let mut say = |line: String| out.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: Some(WaitShow::Meter),
        asker: crate::progress::Asker::Person,
    };
    let next = pending_state(layout, tool, &mut io);
    (out, next)
}

/// State: NOT STARTED — no progress file, no status row. The bump is still written
/// so the next pass front-loads the program the user actually wanted.
#[test]
fn pending_not_started_records_the_wish() {
    let l = temp_layout("pend-notstarted");
    let _ = pending_state_lines(&l, "trust");
    assert_eq!(
        bump_contents(&l),
        "trust unattended\n",
        "the wish is recorded for the next pass, a script's"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A person at a terminal waits: the same live download is a wait that ends in the program
/// running ([`PendingNext::WaitThenRun`]), and a queued program is bumped first so the wait
/// is short. A failed row is said, never waited on.
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

/// An archive the set completion fetched ahead waits, whole, for the pass to reach its
/// program: a person waiting for it bumps it as they would a queued one (its stage has not
/// started), while one whose bytes are still moving is left where it is.
#[test]
fn pending_on_a_terminal_bumps_a_download_that_waits_for_the_pass() {
    let l = temp_layout("pend-wait-ahead");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"ay\":{\"phase\":\"download\",\"bytes_done\":43,\"bytes_total\":43},\
             \"ty\":{\"phase\":\"download\",\"bytes_done\":40,\"bytes_total\":86},\
             \"trust\":{\"phase\":\"extract\"}}",
            "[]",
        ),
    )
    .unwrap();
    let (lines, next) = pending_state_waiting(&l, "ty");
    assert_eq!(next, PendingNext::WaitThenRun, "{lines:?}");
    assert_eq!(bump_contents(&l), "", "moving: not bumped");
    let (lines, next) = pending_state_waiting(&l, "ay");
    assert_eq!(next, PendingNext::WaitThenRun, "{lines:?}");
    assert_eq!(bump_contents(&l), "ay\n", "waiting for the pass: bumped");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A command answers with its program's state: `targo` is `trust`'s, so the progress row
/// read, the bump written and the install named are `trust`'s, and an unattended caller (no
/// terminal) waits for it too, told the bound, not Ctrl-C.
#[test]
fn a_command_stub_reports_and_waits_for_its_program() {
    let l = temp_layout("pend-command");
    std::fs::write(
        l.progress_file(),
        live_snapshot(
            "{\"trust\":{\"phase\":\"download\",\"bytes_done\":4100000,\"bytes_total\":9800000}}",
            "[\"trust\"]",
        ),
    )
    .unwrap();
    let lines = pending_state_lines(&l, "targo");
    assert!(
        lines[0].starts_with("atpkg: targo comes with trust:"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|x| x.contains("trust is being installed (41%, 4.1 of 9.8 MB)")),
        "trust's row, not a missing `targo` row: {lines:?}"
    );
    let mut out: Vec<String> = Vec::new();
    let mut say = |line: String| out.push(line);
    let mut io = PendingIo {
        say: &mut say,
        wait: Some(WaitShow::Lines),
        asker: crate::progress::Asker::Script,
    };
    assert_eq!(
        pending_state(&l, "targo", &mut io),
        PendingNext::WaitThenRun
    );
    assert!(
        out.iter()
            .any(|x| x.contains("targo starts as soon as it is installed")
                && x.contains("waiting at most 15 minutes")),
        "{out:?}"
    );
    // What the wait says every few seconds: the program's share.
    let file = crate::progress::read_progress(&l).unwrap();
    assert_eq!(
        unattended_wait_line("targo", "trust", file.programs.get("trust")),
        "atpkg: targo is being installed (trust 41%)"
    );
    // Nothing installing: the bump names the program the installer can act on.
    let _ = std::fs::remove_file(l.progress_file());
    let lines = pending_state_lines(&l, "trustc");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("nothing is installing right now")),
        "{lines:?}"
    );
    assert_eq!(bump_contents(&l), "trust unattended\n", "{lines:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A WAIT ON A PROMISE ITS CALLER SAW ends at once on a plan that left the program out,
/// even when the update retired the promise before the wait's first read: the stub said
/// "on its way" and then found no promise, so a live default-set pass with no row for
/// `trust` is the honesty edge (127). The pass keeps its heartbeat until the wait has
/// answered, as a real one runs on: a wait that missed the promise waits it out, and the
/// hang detector says so.
#[test]
fn a_wait_on_a_promise_its_caller_saw_answers_a_plan_that_left_it_out_at_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let l = temp_layout("pend-seen-promise");
    // Written whole and renamed in, as a pass writes it: a reader never sees half a plan.
    let plan = |l: &crate::store::Layout| {
        let tmp = l.progress_file().with_extension("tmp");
        std::fs::write(
            &tmp,
            live_snapshot(
                "{\"other\":{\"phase\":\"download\",\"bytes_done\":1,\"bytes_total\":4}}",
                "[\"other\"]",
            ),
        )
        .unwrap();
        std::fs::rename(&tmp, l.progress_file()).unwrap();
    };
    plan(&l);
    assert!(
        !crate::progress::pass_owed(&l, now_secs()),
        "the promise is retired"
    );
    let answered = std::sync::Arc::new(AtomicBool::new(false));
    let heartbeat = {
        let (layout, answered) = ((*l).clone(), std::sync::Arc::clone(&answered));
        std::thread::spawn(move || {
            while !answered.load(Ordering::SeqCst) {
                plan(&layout);
                std::thread::park_timeout(std::time::Duration::from_millis(200));
            }
        })
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let layout = (*l).clone();
    std::thread::spawn(move || {
        let code = wait_then_run(
            &layout,
            "targo",
            &[],
            WaitShow::Lines,
            crate::progress::Asker::Script,
            WaitSeen {
                snapshot: None,
                running: false,
                promise: true,
            },
        );
        let _ = tx.send(code);
    });
    let code = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    answered.store(true, Ordering::SeqCst);
    heartbeat.thread().unpark();
    heartbeat.join().unwrap();
    assert_eq!(
        code.expect("hang detector: the wait never answered while the pass ran"),
        ExitCode::from(127)
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A promised pass is waited for before it begins (`pass-owed.json`, the window's first-run
/// seed or an update about to plan): an unattended `targo` waits for `trust`, bumped first,
/// and keeps waiting through the begun pass's first seconds before its plan lands, where a
/// planned pass that leaves `trust` out is the honesty edge. A promise whose owner is gone,
/// or whose deadline passed, keeps nobody waiting: the not-running answer, as with no
/// promise at all.
#[test]
fn a_promised_pass_is_waited_for_until_it_plans() {
    fn lines_waiting_unattended(l: &crate::store::Layout) -> (Vec<String>, PendingNext) {
        let mut out: Vec<String> = Vec::new();
        let mut say = |line: String| out.push(line);
        let mut io = PendingIo {
            say: &mut say,
            wait: Some(WaitShow::Lines),
            asker: crate::progress::Asker::Script,
        };
        let next = pending_state(l, "targo", &mut io);
        (out, next)
    }
    let l = temp_layout("pend-promised");
    crate::progress::promise_pass(&l, std::process::id(), crate::progress::SEED_PROMISE_SECS);
    let (lines, next) = lines_waiting_unattended(&l);
    assert_eq!(next, PendingNext::WaitForPromise, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("starting your toolchain install")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|x| x.contains("trust is on its way")
            && x.contains("targo starts as soon as it is installed")
            && x.contains("waiting at most 15 minutes")),
        "{lines:?}"
    );
    assert_eq!(
        bump_contents(&l),
        "trust unattended\n",
        "bumped before the wait, for a script"
    );
    // A caller that cannot wait is told the program is requested first.
    let lines = pending_state_lines(&l, "targo");
    assert!(
        lines.iter().any(|x| x.contains("requested first")),
        "{lines:?}"
    );

    // The pass begun, its plan not in yet: still the promise's wait, never the edge…
    std::fs::write(l.progress_file(), live_snapshot("{}", "[]")).unwrap();
    let (lines, next) = lines_waiting_unattended(&l);
    assert_eq!(next, PendingNext::WaitForPromise, "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|x| x.contains("no longer part of the default set")),
        "{lines:?}"
    );
    // …which the same planned pass, the promise retired, is (the negative control).
    crate::progress::retire_promise(&l);
    let (lines, next) = lines_waiting_unattended(&l);
    assert_eq!(next, PendingNext::Done, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("no longer part of the default set")),
        "{lines:?}"
    );
    let _ = std::fs::remove_file(l.progress_file());

    // A broken promise: its owner gone, or past its deadline.
    let gone = std::process::Command::new("/usr/bin/true")
        .spawn()
        .map(|mut c| {
            let pid = c.id();
            let _ = c.wait();
            pid
        })
        .expect("a reaped child's pid");
    crate::progress::promise_pass(&l, gone, crate::progress::SEED_PROMISE_SECS);
    let (lines, next) = lines_waiting_unattended(&l);
    assert_eq!(next, PendingNext::Done, "owner gone: {lines:?}");
    assert!(
        lines
            .iter()
            .any(|x| x.contains("nothing is installing right now")),
        "{lines:?}"
    );
    std::fs::write(
        l.pass_owed_file(),
        format!(
            "{{\"v\":1,\"owner\":{},\"until_unix\":{}}}",
            std::process::id(),
            now_secs() - 1
        ),
    )
    .unwrap();
    let (lines, next) = lines_waiting_unattended(&l);
    assert_eq!(next, PendingNext::Done, "deadline passed: {lines:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A failed pass left before the command began is not the promised successor,
/// including when both clocks round to the same Unix second.
#[test]
fn a_promised_wait_does_not_adopt_an_unchanged_same_second_failure() {
    let l = temp_layout("pending-same-second-old-failure");
    let began = now_secs();
    let sink = crate::progress::ProgressSink::create(&l.progress_file(), "net").unwrap();
    sink.plan(&[("trust".into(), 1)]);
    sink.finished(
        "trust",
        crate::progress::Phase::Failed,
        Some("old failure".into()),
    );
    sink.finish();
    let mut initial = crate::progress::read_progress(&l).unwrap();
    // An exact controlled second, without waiting for a wall-clock boundary.
    initial.started_unix = began;
    initial.heartbeat_unix = began;
    initial.ended_unix = Some(began);
    std::fs::write(l.progress_file(), aterm_json::to_string(&initial).unwrap()).unwrap();
    crate::progress::promise_pass(&l, std::process::id(), crate::progress::SEED_PROMISE_SECS);
    let owed = crate::progress::pass_owed(&l, began);
    assert!(owed);
    let snapshot = crate::progress::read_progress(&l).unwrap();
    let selected = pending_wait_file(Some(&snapshot), Some(&initial), false, false);
    let failed = selected
        .and_then(|file| file.programs.get("trust"))
        .is_some_and(|row| row.phase == crate::progress::Phase::Failed);
    assert_eq!(pending_wait_step(owed, false, failed), WaitStep::Wait);
}

/// A real successor that begins and ends between looks still owns its failure,
/// even if every coarse timestamp and program row equals the preceding pass.
#[test]
fn a_pending_wait_reports_a_same_second_successor_failure() {
    let l = temp_layout("pending-same-second-new-failure");
    let began = now_secs();
    let finish = || {
        let sink = crate::progress::ProgressSink::create(&l.progress_file(), "net").unwrap();
        sink.plan(&[("trust".into(), 1)]);
        sink.finished(
            "trust",
            crate::progress::Phase::Failed,
            Some("same reason".into()),
        );
        sink.finish();
        let mut file = crate::progress::read_progress(&l).unwrap();
        file.started_unix = began;
        file.heartbeat_unix = began;
        file.ended_unix = Some(began);
        std::fs::write(l.progress_file(), aterm_json::to_string(&file).unwrap()).unwrap();
        file
    };
    let initial = finish();
    crate::progress::promise_pass(&l, std::process::id(), crate::progress::SEED_PROMISE_SECS);
    assert!(crate::progress::pass_owed(&l, began));
    let successor = finish();
    assert!(initial.generation.is_some());
    assert_ne!(
        initial.generation, successor.generation,
        "a new pass has its own identity"
    );
    crate::progress::retire_promise(&l);
    let owed = crate::progress::pass_owed(&l, began);
    assert!(!owed);
    let selected = pending_wait_file(Some(&successor), Some(&initial), false, false)
        .expect("the successor terminal snapshot belongs to this wait");
    assert_eq!(
        selected.programs["trust"].phase,
        crate::progress::Phase::Failed
    );
    assert_eq!(pending_wait_step(owed, false, true), WaitStep::Fail);
}

#[test]
fn a_legacy_terminal_snapshot_requires_an_observed_live_pass() {
    let began = now_secs();
    let live: crate::progress::ProgressFile = aterm_json::from_str(&format!(
        "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"started_unix\":{began},\
         \"heartbeat_unix\":{began},\"programs\":{{\"trust\":{{\"phase\":\"queued\"}}}}}}",
        std::process::id()
    ))
    .unwrap();
    assert!(
        live.generation.is_none(),
        "an older writer supplies no generation"
    );
    let mut ended = live.clone();
    ended.pid = None;
    ended.ended_unix = Some(began);
    ended.programs.get_mut("trust").unwrap().phase = crate::progress::Phase::Failed;
    assert!(pending_wait_file(Some(&ended), Some(&live), false, true).is_some());
    assert!(pending_wait_file(Some(&ended), Some(&ended), false, false).is_none());
    assert!(pending_wait_file(Some(&ended), None, false, false).is_none());
    ended.generation = Some("unrecognized-identity".into());
    assert!(pending_wait_file(Some(&ended), None, false, false).is_none());
}

/// A whole update promises its plan on a first install, or over a promise it finds. A
/// routine pass (an ALab program already active) makes none, so a stub for a program it
/// will not plan (held, unserved here) answers with its recorded reason at once instead of
/// waiting for that plan; it takes over the window seed's live promise; and a pass that
/// does not plan the set promises nothing.
#[test]
fn a_whole_update_promises_its_plan_only_on_a_first_install_or_over_a_promise() {
    let l = temp_layout("update-promise-gate");
    let now = now_secs();
    assert!(update_promises_its_plan(&l, true, now), "a first install");
    assert!(!update_promises_its_plan(&l, false, now), "no set to plan");
    make_live(&l, "trust", 1);
    assert!(!update_promises_its_plan(&l, true, now), "a routine pass");
    crate::progress::promise_pass(&l, std::process::id(), crate::progress::SEED_PROMISE_SECS);
    assert!(
        update_promises_its_plan(&l, true, now),
        "the seed's promise, taken over"
    );
    assert!(!update_promises_its_plan(&l, false, now));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The promise ends where the plan begins: a pass promised before it began retires the
/// promise once its plan is in `progress.json`, the pass still live, so a stub reads the
/// pass's own rows from then on. A pass that stops before it plans (its index
/// unresolvable) leaves the promise to the update's return ([`PromiseEnds`]).
#[test]
fn a_promised_pass_retires_its_promise_once_its_plan_lands() {
    let _slot = crate::progress::own_pass_slot();
    let now = || aterm_time::unix_secs().unwrap();
    let cfg = crate::config::PackagesConfig::default();
    // Negative control: nothing to resolve, so no plan, and the promise stands.
    let empty = scratch("promise-plan-empty");
    let layout = temp_layout("promise-plan-unresolved");
    crate::progress::promise_pass(&layout, std::process::id(), 60);
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let out = install_default_set(
        &layout,
        &CountingFetcher::new(empty.clone()),
        &test_anchor(),
        &cfg,
        0,
    );
    crate::progress::end_pass();
    assert!(out.index_error.is_some(), "the fixture resolves nothing");
    assert!(
        crate::progress::pass_owed(&layout, now()),
        "no plan landed, so the promise stands"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);

    let dir = scratch("promise-plan");
    write_registry(&dir, "stable");
    let layout = temp_layout("promise-plan");
    crate::progress::promise_pass(&layout, std::process::id(), 60);
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let out = install_default_set(
        &layout,
        &CountingFetcher::new(dir.clone()),
        &test_anchor(),
        &cfg,
        0,
    );
    let live = crate::progress::live_snapshot().expect("the pass is still live");
    crate::progress::end_pass();
    assert_eq!(out.index_error, None);
    assert!(
        !live.queue.is_empty() || !live.programs.is_empty(),
        "the plan landed"
    );
    assert!(
        !crate::progress::pass_owed(&layout, now()) && !layout.pass_owed_file().exists(),
        "and the promise is retired"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// THE WINDOW'S SEED SAYS THE RECORDED VERDICT ON A LAUNCH NO UPDATE FOLLOWS. A toolset
/// excluded to nothing gets its `seed-unusable:` card from the update's empty-store arm, and
/// the window's seed asks no index; on a launch whose update is not due, that seed says the
/// verdict again from the record, so the card is there at every launch. Not when the update
/// is due (it says it), not once a later pass's outcome replaced the verdict, and not on a
/// store something is wanted in.
#[test]
fn the_window_seed_says_the_recorded_verdict_when_no_update_is_due() {
    let layout = temp_layout("seed-recorded-verdict");
    let narrowed = crate::config::PackagesConfig {
        exclude: Some(
            crate::vendor_direct::VENDORS
                .iter()
                .map(|spec| spec.program.to_string())
                .chain(["ay".to_string()])
                .collect(),
        ),
        ..Default::default()
    };
    let (index_bytes, sig) = crate::vendor_direct::lane::world::signed_index_naming(
        44,
        "2099-01-01T00:00:00Z",
        &[],
        &[],
    );
    let (roster_bytes, roster_sig) = testkit::published_roster();
    let candidate = crate::Candidate {
        label: "seed-recorded-verdict".to_string(),
        index_bytes,
        sig,
        roster_bytes,
        roster_sig,
    };
    let source = format!(
        "github:{}",
        crate::resolve_account(narrowed.account()).slug()
    );
    let cache = crate::IndexCache::for_layout(&layout);
    cache.store(&source, std::slice::from_ref(&candidate));
    let read_cached = || {
        crate::select_index(
            &test_anchor(),
            cache.load(&source)?,
            build_floor(&layout),
            testkit::NOW,
        )
        .selected
        .map(|selected| selected.index)
    };
    assert!(read_cached().is_some(), "the signed local fixture admits");
    let why = "[packages].exclude narrows the toolset to nothing installable on this machine";
    record_unusable_toolset(&layout, crate::state::TOOLSET_EXCLUDED, why);
    let now = now_secs() as i64;
    crate::status::stamp_success(&layout, &rfc3339_at(now)).unwrap();
    let _ = take_markers_said();
    report_seedless_posture_as(&layout, &narrowed, true, now, read_cached);
    assert_eq!(take_markers_said(), [seed_unusable_line(why)]);
    let walk = aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS as i64;
    report_seedless_posture_as(&layout, &narrowed, true, now + walk, || {
        panic!("a due update does not read the local replay cache")
    });
    assert!(
        take_markers_said().is_empty(),
        "the update that is due says it"
    );
    report_seedless_posture_as(
        &layout,
        &crate::config::PackagesConfig::default(),
        true,
        now,
        || panic!("a served store does not read the local replay cache"),
    );
    assert!(take_markers_said().is_empty(), "the agents are wanted here");
    record_unusable_toolset(
        &layout,
        crate::state::TOOLSET_UNSERVED,
        "no build for this target",
    );
    report_seedless_posture_as(&layout, &narrowed, true, now, || {
        panic!("an unserved architecture needs no local replay cache")
    });
    assert_eq!(
        take_markers_said(),
        [seed_unusable_line("no build for this target")]
    );
    record_outcome(&layout, "up to date (index build 44)".to_string());
    report_seedless_posture_as(&layout, &narrowed, true, now, || {
        panic!("a moved-on record does not read the local replay cache")
    });
    assert!(take_markers_said().is_empty(), "the record moved on");

    record_unusable_toolset(&layout, crate::state::TOOLSET_EXCLUDED, why);
    std::fs::remove_file(layout.prefix.join("index-cache.toml")).unwrap();
    report_seedless_posture_as(&layout, &narrowed, true, now, read_cached);
    let without_cache = take_markers_said();
    cache.store(&source, std::slice::from_ref(&candidate));
    assert!(
        read_cached().is_some(),
        "the same signed cache was restored"
    );
    let ay_wanted = crate::config::PackagesConfig {
        exclude: Some(
            crate::vendor_direct::VENDORS
                .iter()
                .map(|spec| spec.program.to_string())
                .collect(),
        ),
        ..Default::default()
    };
    report_seedless_posture_as(&layout, &ay_wanted, true, now, read_cached);
    let after_reenabling_ay = take_markers_said();
    assert_eq!(
        [without_cache, after_reenabling_ay],
        [Vec::<String>::new(), Vec::new()],
        "ATPKG_REPLAYED_AN_EMPTY_SET_WITHOUT_CURRENT_LOCAL_POLICY"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// TIER-1: the stub's REAL decision ([`pending_wait_step`]) against the derived model
/// `AtpkgPendingWait`, at every reachable deciding state — the model's reads mapped
/// onto the function's inputs in the order `wait_then_run` makes them (the promise, then
/// the pass, then the shim). Negative control: the decision the stub could have made
/// instead — "the pass ended, so it failed", asked before the shim — disagrees with the
/// model at a reachable state, so agreement is never vacuous.
#[test]
fn the_pending_wait_decision_refines_the_derived_model() {
    use aterm_models_atpkg::atpkg_pending_wait_model;
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
    let deciding: Vec<_> = seen.iter().filter(|s| s["step"] == 2).collect();
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
    // `running` is what the promise and the pass read together say (a pass is coming), and
    // `failed` is the pass saying it will not bring the program: a failed row, a plan that
    // left it out (`left_out`), or a done row with no shim of the name (`landed_without`).
    let inputs = |s: &std::collections::BTreeMap<&'static str, i64>| {
        (
            s["coming"] == 1,
            s["shim"] == 1,
            s["row"] == 4 || s["left"] == 1 || s["done"] == 1,
        )
    };
    for s in &deciding {
        let (running, installed, failed) = inputs(s);
        assert_eq!(
            pending_wait_step(running, installed, failed),
            modeled(s),
            "the real decision at {s:?}"
        );
    }
    // Negative control: ended-means-failed, asked before the shim.
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
}

/// A pass that installs something else (a person's `update`, another program's `install`)
/// is no delisting: its snapshot never planned the default set, so the stub answers as if
/// nothing were installing this program, and still records the wish.
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
    assert_eq!(bump_contents(&l), "trust unattended\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// State: skipped, a terminal phase, never a wait. `Sink::finished` has retired the program
/// from the queue and counted it done, so nothing later in this pass installs it and the
/// reorder-only bump file cannot put it back. The stub says the recorded reason
/// (control-stripped) and claims no bump.
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

/// State: failed. The recorded error surfaces with its next act, control characters
/// stripped before the TTY.
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

/// A stale snapshot renders only not-running states: yesterday's "downloading" is never
/// today's claim.
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

/// The honesty edge: a live resolved-pass snapshot that does not plan the program says "no
/// longer part of the default set" and claims no bump, which the reorder-only intersection
/// would drop.
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
    assert!(
        lines.len() == 1 && !lines[0].contains("sudo"),
        "one refusal, the name never echoed: {lines:?}"
    );
    assert_eq!(bump_contents(&l), "");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A pending stub an older client laid for an extra (with the retired marker line) reads
/// as the default-set state: every program the index names is a default-set member now, so
/// `__pending` says it is not installed yet and bumps it, with no question or marker. An
/// agent program with no stub gets its vendor line the same way.
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
    assert_eq!(bump_contents(&l), "vendorx unattended\n");
    let lines = pending_state_lines(&l, "claude");
    assert!(lines[0].contains("not installed yet"), "{lines:?}");
    assert_eq!(bump_contents(&l), "vendorx unattended\nclaude unattended\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

// The priority queue — reorder-only by construction (design §4).

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

/// Bumping any member bumps its whole group: the tuple stays transactional, the queue
/// jump is group-granular.
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

/// `verify-pkg` runs the client's whole chain, not only the signature: a publisher that
/// takes it as the client's own rule refuses to ship what it refuses, so a manifest that
/// verifies but no client can parse must fail it. Every body is signed by the roster's own
/// machine, so only the post-verify behaviour is under test; the refusal names the fix, as
/// `Reject::ShimEnv`/`RetiredKind` promise.
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

    // The verb, over a manifest machine-signed through the real chain every time.
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

    // A manifest a client can read passes.
    assert_eq!(
        run("good", &format!("{HEAD}{ROW}")),
        verified_ok,
        "a signed, parsable manifest must still verify"
    );

    // Every post-verify gate a client applies fails the verb too.
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

/// `verify-index` names a post-verify mistake instead of blaming the signature.
/// `TrustedRoster::authorize_index` verifies Ed25519 first, then parses, then binds the
/// attribution, so `Malformed`, `Schema`, `Unattributed` and `SeqMismatch` are reachable
/// only once a roster machine signed these bytes; reported as "no machine on that roster
/// signed this index" they would send the publisher to re-key when the fix is one line of
/// the index. Every body is signed by the roster's own machine; the last case guards the
/// other direction: a crypto verdict stays opaque (§8).
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
    // What the verb prints for those bytes: the refusal the same call hands it.
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

    // A correctly authored index verifies.
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
        // A misread `roster_seq` lands here, and the refusal must name it.
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

    // The other direction: a signature that does not verify says nothing beyond that,
    // so the operator gets the one opaque line (§8).
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
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
    }
}

/// A `dir:` registry whose `rustc` tuple is `trust` + `tb`, pinned at trust 4821 and
/// tb 6 by an index still valid at the world's clock, beside both signed pkgs.
fn write_toolchain_registry(dir: &Path) {
    write_toolchain_registry_and(dir, &[]);
}

/// [`write_toolchain_registry`] with the ungrouped `singletons` `(program, build)` beside
/// the tuple.
fn write_toolchain_registry_and(dir: &Path, singletons: &[(&str, u64)]) {
    let programs: String = singletons
        .iter()
        .map(|(p, _)| format!("[programs.{p}]\nrepo = \"{p}\"\n"))
        .collect();
    let pins: String = singletons
        .iter()
        .map(|(p, b)| format!(", {p} = {b}"))
        .collect();
    let body = format!(
        "schema = 2\nindex_build = 51\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         [programs.tb]\nrepo = \"tb\"\ncoherence_group = \"rustc\"\n\
         {programs}\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n\
         pin = {{ trust = 4821, tb = 6{pins} }}\n",
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
    for (p, b) in singletons {
        write_pkg(dir, p, *b);
    }
}

/// One flip gate for the whole unattended pass: with a build running from trust 4800, the
/// apply lane stages the `rustc` tuple and waits, and the same pass's set-completion lane,
/// finding `tb` absent, must not complete it by flipping trust under that build. `tb`
/// arrives with the flip once the toolchain is quiet.
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

/// A pass that only applies (no set completion: `auto_install = false`, a machine nothing
/// adopted) and flips trust 4800 to 4821 leaves the rustup view presenting 4821.
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

/// The ALab set and the agents in one fetcher, worked from several threads at once as the
/// production fetcher is ([`crate::flow::Fetcher::shared`]): a `dir:` registry for the set
/// and a vendor world for the agents. With `checkpoints`, three requests wait, each on the
/// one-minute hang detector, for what only a pass running its lanes side by side asks
/// first: claude's head for codex's (the vendor plans go out together), the first agent's
/// payload for the other lane's landing to wait on the link (the lanes land side by side),
/// and the pass's read of a `rustc` member's manifest once the tuple's archives are moving,
/// for claude's payload (the agents land while the set installs, not once its toolchain has
/// flipped).
struct SetBesideVendors {
    registry: crate::DirFetcher,
    vendors: crate::vendor_direct::lane::world::Fake,
    layout: crate::store::Layout,
    checkpoints: bool,
    /// Every `head:<program>`, `download:<program>` and `payload:<program>` asked so far.
    asked: (std::sync::Mutex<Vec<String>>, std::sync::Condvar),
    /// As each agent's payload was asked for: whether every archive of `set` was in, and
    /// whether trust was live.
    at_payload: std::sync::Mutex<Vec<(String, bool, bool)>>,
    /// The set's programs, whose archives the agents' bytes wait for.
    set: &'static [&'static str],
    /// The live pass's queue as the set's first archive was asked for.
    queue_at_first_download: std::sync::Mutex<Option<Vec<String>>>,
    /// trust's archive waits, on the one-minute hang detector, for claude's payload: what
    /// only a pass that lands a bumped agent beside the tuple asks first.
    tuple_waits_for_claude: bool,
    /// Agent payloads downloading now, and the most there ever were at once.
    payloads_in_flight: (
        std::sync::atomic::AtomicUsize,
        std::sync::atomic::AtomicUsize,
    ),
    /// As each agent's payload was asked for: whether a pass was still promised, and the
    /// live plan's rows.
    owed_at_payload: std::sync::Mutex<Vec<(String, bool, Vec<String>)>>,
    /// The set's programs whose first archive download fails, as a dropped transfer does.
    flaky: std::sync::Mutex<Vec<&'static str>>,
    /// Every set archive download, with the thread that asked.
    downloads: std::sync::Mutex<Vec<(String, String)>>,
    /// The index resolve waits — on the one-minute hang detector — for this to be asked.
    resolve_waits_for: Option<&'static str>,
    /// `(program, what)`: that program's head read waits, on the hang detector, for `what`.
    head_waits_for: Option<(&'static str, &'static str)>,
    /// `(program, agent)`: that program's payload waits, on the hang detector, until `agent`'s
    /// shim is live and its status row reads `managed` — the row its lane's landing writes.
    payload_waits_for_row: Option<(&'static str, &'static str)>,
    /// The set's manifest prefetch waits, on the hang detector, until the store's index floor
    /// is recorded.
    prefetch_waits_for_floor: bool,
    /// The set's manifest prefetch waits, on the hang detector, for this to be asked.
    prefetch_waits_for: Option<&'static str>,
}

impl SetBesideVendors {
    fn new(registry: &Path, layout: &crate::store::Layout, checkpoints: bool) -> Self {
        Self {
            registry: crate::DirFetcher::new(registry.to_path_buf()),
            vendors: crate::vendor_direct::lane::world::Fake::default(),
            layout: layout.clone(),
            checkpoints,
            asked: Default::default(),
            at_payload: std::sync::Mutex::default(),
            set: &["tb", "trust"],
            queue_at_first_download: std::sync::Mutex::default(),
            tuple_waits_for_claude: false,
            payloads_in_flight: Default::default(),
            owed_at_payload: std::sync::Mutex::default(),
            flaky: std::sync::Mutex::default(),
            downloads: std::sync::Mutex::default(),
            resolve_waits_for: None,
            head_waits_for: None,
            payload_waits_for_row: None,
            prefetch_waits_for_floor: false,
            prefetch_waits_for: None,
        }
    }

    /// Wait — on the one-minute hang detector — until `agent`'s shim is live (no pending stub
    /// stands there) and its status row reads `managed`.
    fn wait_until_row_reads_managed(&self, agent: &str) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let tool = crate::store::ToolName::new(agent).unwrap();
        loop {
            let live = crate::which(&self.layout, agent).is_some()
                && !crate::stub::is_pending_stub(&self.layout.agent_shim(&tool));
            let row = crate::status::read(&self.layout)
                .and_then(|status| status.programs.get(agent).map(|row| row.state.clone()));
            if live
                && row
                    .as_deref()
                    .is_some_and(|state| state.starts_with("managed"))
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{agent}'s row never read managed while its shim was live: {row:?} (live: {live})"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Wait, on the one-minute hang detector, until the live pass's row for `program` says
    /// its landing has begun (its build is noted) and it still waits, queued, for its bytes.
    fn wait_until_waiting_for_the_link(&self, program: &str) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !crate::progress::live_snapshot()
            .and_then(|file| file.programs.get(program).cloned())
            .is_some_and(|row| row.build.is_some() && row.phase == crate::progress::Phase::Queued)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{program}'s landing never waited for the link"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn note(&self, what: String) {
        self.asked.0.lock().unwrap().push(what);
        self.asked.1.notify_all();
    }

    fn wait_for(&self, what: &str) {
        if self.checkpoints {
            self.wait_until(what);
        }
    }

    fn was_asked(&self, what: &str) -> bool {
        self.asked.0.lock().unwrap().iter().any(|a| a == what)
    }

    fn wait_until(&self, what: &str) {
        let asked = self.asked.0.lock().unwrap();
        let (_asked, wait) = self
            .asked
            .1
            .wait_timeout_while(asked, aterm_spec::wait::MUST_HAPPEN, |asked| {
                !asked.iter().any(|a| a == what)
            })
            .unwrap();
        assert!(!wait.timed_out(), "{what} was never asked for");
    }
}

impl crate::flow::Fetcher for SetBesideVendors {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        if let Some(what) = self.resolve_waits_for {
            self.wait_until(what);
        }
        self.registry.index_candidates()
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        if matches!(program, "trust" | "tb")
            && (self.was_asked("download:trust") || self.was_asked("download:tb"))
        {
            self.wait_for("payload:claude");
        }
        self.registry.pkg_manifest(repo, program, build)
    }

    fn prefetch_manifests(&self, pins: &[(String, String, u64)]) {
        if self.prefetch_waits_for_floor {
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while crate::sig::Floor::new(self.layout.floor()).current() == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the floor was never recorded while the set's manifests were out"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        if let Some(what) = self.prefetch_waits_for {
            self.wait_until(what);
        }
        for (_, program, _) in pins {
            self.note(format!("prefetch:{program}"));
        }
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.registry.download(repo, asset, dest)
    }

    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
        _: u64,
    ) -> Result<(), String> {
        self.note(format!("download:{program}"));
        self.queue_at_first_download
            .lock()
            .unwrap()
            .get_or_insert_with(|| {
                crate::progress::live_snapshot().map_or_else(Vec::new, |file| file.queue)
            });
        if program == "trust" && self.tuple_waits_for_claude {
            self.wait_until("payload:claude");
        }
        self.downloads.lock().unwrap().push((
            program.to_string(),
            std::thread::current().name().unwrap_or("").to_string(),
        ));
        let dropped = {
            let mut flaky = self.flaky.lock().unwrap();
            let first = flaky.iter().position(|p| *p == program);
            first.map(|i| flaky.remove(i)).is_some()
        };
        if dropped {
            return Err("curl: (56) Recv failure: Connection reset by peer".into());
        }
        let fetched = self.registry.download(repo, asset, dest);
        if fetched.is_ok() {
            self.note(format!("downloaded:{program}"));
        }
        fetched
    }

    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.note(format!("get:{program}"));
        self.vendors.vendor_get(program, url, cap, if_none_match)
    }

    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.note(format!("head:{program}"));
        if program == "claude" {
            self.wait_for("head:codex");
        }
        if let Some((waiting, what)) = self.head_waits_for
            && waiting == program
        {
            self.wait_until(what);
        }
        self.vendors.vendor_head(program, url, cap, if_none_match)
    }

    fn vendor_content_length(
        &self,
        program: &str,
        url: &str,
    ) -> Result<u64, crate::flow::VendorFetchError> {
        self.vendors.vendor_content_length(program, url)
    }

    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), crate::flow::VendorFetchError> {
        self.at_payload.lock().unwrap().push((
            program.to_string(),
            self.set
                .iter()
                .all(|p| self.was_asked(&format!("downloaded:{p}"))),
            crate::active_builds(&self.layout).contains_key("trust"),
        ));
        self.owed_at_payload.lock().unwrap().push((
            program.to_string(),
            crate::progress::pass_owed(&self.layout, now_secs()),
            crate::progress::live_snapshot()
                .map(|file| file.programs.into_keys().collect())
                .unwrap_or_default(),
        ));
        if let Some((waiting, agent)) = self.payload_waits_for_row
            && waiting == program
        {
            self.wait_until_row_reads_managed(agent);
        }
        use std::sync::atomic::Ordering::SeqCst;
        let (now, most) = &self.payloads_in_flight;
        most.fetch_max(now.fetch_add(1, SeqCst) + 1, SeqCst);
        self.note(format!("payload:{program}"));
        let other = if program == "claude" {
            "codex"
        } else {
            "claude"
        };
        if self.checkpoints && !self.was_asked(&format!("payload:{other}")) {
            // The other lane's landing begins while the first agent's bytes are in flight,
            // and waits for the link, its row still queued: the lanes land side by side, and
            // only their bytes take turns.
            self.wait_until_waiting_for_the_link(other);
        }
        let paid = self.vendors.vendor_download(program, url, dest, cap);
        now.fetch_sub(1, SeqCst);
        paid
    }

    fn source_id(&self) -> String {
        self.registry.source_id()
    }

    fn shared(&self) -> Option<&(dyn crate::flow::Fetcher + Sync)> {
        Some(self)
    }
}

/// The agents a first run lands beside its set: claude and codex (agy and muse, which the
/// vendor world does not publish, left out).
fn claude_and_codex_wanted() -> crate::config::PackagesConfig {
    crate::config::PackagesConfig {
        exclude: Some(agents_except(&["claude", "codex"])),
        ..Default::default()
    }
}

/// Every agent program of the table but `keep`: what a test wanting only those excludes, so
/// a row the table gains (`codex-old`) never joins a test that did not ask for it.
fn agents_except(keep: &[&str]) -> Vec<String> {
    crate::vendor_direct::VENDORS
        .iter()
        .map(|spec| spec.program)
        .filter(|program| !keep.contains(program))
        .map(String::from)
        .collect()
}

/// A first run lands its agents beside its set under one announcement. On an empty store
/// whose index resolves, the vendor plans go out together; no agent's payload is asked for
/// until every archive of the set is in (a pass landing the agents first, or beside the
/// set's own downloads, fails here), and the agents then land while the set installs (the
/// pass's read of a `rustc` manifest at its stage waits for claude's payload, so a pass
/// holding the agents until the toolchain has flipped never finishes), two at a time,
/// taking turns at the download alone: codex's lane reaches its download while claude's is
/// in flight, and its bytes move only once claude's are in (two payloads at once fail here,
/// and a lane that held its turn past the download never lets codex's row move). One
/// `net-starting:` names both lanes before any byte, one terminal answers it with both,
/// every row is its own lane's, no stub stands over an agent's shim, and the rustup view
/// follows trust at its flip and again at the pass end.
#[test]
fn an_empty_store_lands_its_agents_beside_its_set_under_one_announcement() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("beside-registry");
    write_toolchain_registry_and(&dir, &[("ay", 18)]);
    let layout = temp_layout("beside-store");
    let mut f = SetBesideVendors::new(&dir, &layout, true);
    f.set = &["ay", "tb", "trust"];
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let views = std::sync::Mutex::new(Vec::new());
    let seam = |layout: &crate::store::Layout| {
        views
            .lock()
            .unwrap()
            .push(crate::active_builds(layout).get("trust").copied());
    };
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.rustup_seam = &seam;
    let _ = take_markers_said();
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    crate::progress::end_pass();
    assert_eq!(code, 0);
    assert_eq!(
        f.payloads_in_flight
            .1
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one agent's bytes at a time"
    );
    let markers = take_markers_said();
    let starts: Vec<&String> = markers
        .iter()
        .filter(|m| m.contains(NET_STARTING_MARKER))
        .collect();
    assert_eq!(starts.len(), 1, "one announcement: {markers:?}");
    assert!(
        starts[0].starts_with(&format!(
            "atpkg: {NET_STARTING_MARKER}installing 5 program(s) over the network: claude, \
             codex, ay, tb, trust — "
        )),
        "{}",
        starts[0]
    );
    let terminals: Vec<&String> = markers
        .iter()
        .filter(|m| {
            m.contains(NET_INSTALLED_MARKER)
                || m.contains(NET_FAILED_MARKER)
                || m.contains(SEED_DONE_MARKER)
        })
        .collect();
    assert_eq!(
        terminals,
        [&format!(
            "atpkg: {NET_INSTALLED_MARKER}ay, claude, codex, tb, trust"
        )],
        "one terminal answers it"
    );
    let at_payload = f.at_payload.lock().unwrap().clone();
    assert_eq!(
        at_payload
            .iter()
            .map(|(agent, tuple_in, _)| (agent.as_str(), *tuple_in))
            .collect::<Vec<_>>(),
        [("claude", true), ("codex", true)],
        "the agents' bytes wait for the set's, and move in the order the lanes took them: \
         claude, taken first, has the link first"
    );
    assert!(
        !at_payload[0].2,
        "claude's bytes move before the toolchain flips"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        (active.get("trust"), active.get("tb"), active.get("ay")),
        (Some(&4821), Some(&6), Some(&18))
    );
    let rows = crate::status::read(&layout).unwrap().programs;
    assert_eq!(rows["claude"].state, "managed 2.1.281 — Anthropic latest");
    assert_eq!(rows["codex"].state, "managed 0.157.0 — OpenAI latest");
    assert_eq!(rows["trust"].installed_build, Some(4821));
    for agent in ["claude", "codex"] {
        assert!(
            !crate::stub::pending_stub_exists(&layout, agent),
            "{agent}: its shim, not a stub"
        );
    }
    assert_eq!(
        *views.lock().unwrap(),
        [Some(4821), Some(4821)],
        "the view follows trust at its flip, and the pass end re-lays it"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A first run's queue reads in the order its bytes move, and the agents' lane meters on
/// the pass it rides: while the `rustc` tuple downloads, the queue a pending stub reads its
/// place in names the tuple first and the agents after it (never claude "next in line"
/// ahead of a toolchain it waits for), and each agent's row, landed on a thread of its own,
/// ends `done` on the one live pass.
#[test]
fn a_first_runs_queue_names_the_toolchain_before_the_agents_that_wait_for_it() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("beside-queue-registry");
    write_toolchain_registry_and(&dir, &[("ay", 18)]);
    let layout = temp_layout("beside-queue-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    crate::progress::append_bump(
        &layout,
        &crate::store::ToolName::new("trust").unwrap(),
        crate::progress::Asker::Person,
    )
    .unwrap();
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    let file = crate::progress::live_snapshot().expect("the pass is live");
    crate::progress::end_pass();
    assert_eq!(code, 0);
    assert_eq!(
        f.queue_at_first_download.lock().unwrap().as_deref(),
        Some(&["tb", "trust", "claude", "codex", "ay"].map(String::from)[..])
    );
    for agent in ["claude", "codex"] {
        assert_eq!(
            file.programs[agent].phase,
            crate::progress::Phase::Done,
            "{agent}"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A person who typed an agent's name does not wait for the toolchain. Its pending stub
/// bumps it ("bumped first when still queued, so the wait is short"); the agents ride
/// behind the `rustc` tuple, so the bumped one lands at once beside it (trust's archive
/// waits for claude's payload, and a lane that ignored the bump never asks for it), while
/// an agent nobody asked for still waits for the set's archives.
#[test]
fn a_bumped_agent_lands_beside_the_toolchain_not_after_it() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("beside-bump-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("beside-bump-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.tuple_waits_for_claude = true;
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    crate::progress::append_bump(
        &layout,
        &crate::store::ToolName::new("claude").unwrap(),
        crate::progress::Asker::Person,
    )
    .unwrap();
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    assert_eq!(
        f.at_payload
            .lock()
            .unwrap()
            .iter()
            .map(|(agent, tuple_in, _)| (agent.as_str(), *tuple_in))
            .collect::<Vec<_>>(),
        [("claude", false), ("codex", true)],
        "claude, asked for, beside the tuple; codex after the set's archives"
    );
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p))
    );
    assert!(
        crate::progress::read_bump(&layout).is_empty(),
        "the completed request is consumed"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RIDING AGENT'S ROW IS WRITTEN AS IT LANDS, not once the pass is over. The agents land
/// on lane threads beside the set, and the pass's thread — the one that writes every row —
/// reports each landing as it comes: codex's payload, the other lane's, waits on the
/// one-minute hang detector until claude's shim is live and claude's status row reads
/// `managed`. A pass that writes the riders' rows only after its lanes join never lets
/// codex's bytes move.
#[test]
fn a_riding_agents_row_is_written_as_it_lands_while_the_other_lane_still_runs() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("beside-row-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("beside-row-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.payload_waits_for_row = Some(("codex", "claude"));
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let rows = crate::status::read(&layout).unwrap().programs;
    assert_eq!(rows["claude"].state, "managed 2.1.281 — Anthropic latest");
    assert_eq!(rows["codex"].state, "managed 0.157.0 — OpenAI latest");
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE EXPLICIT VERB LANDS ITS AGENTS BESIDE ITS SET, as a first run's update pass does.
/// `install --default-set` on an empty store asks for no agent's payload until every
/// archive of the set is in — the vendor-first verb landed claude and codex before the
/// set's first byte — under ONE announcement that keeps the verb's own consent (it names
/// neither the first launch nor `auto_install`), answered once; each agent's row is written
/// as it lands (codex's payload waits for claude's, as above), and the rustup view follows
/// trust at its flip as well as at the verb's end.
#[test]
fn the_toolset_verb_lands_its_agents_beside_its_set_under_one_announcement() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("verb-beside-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("verb-beside-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.payload_waits_for_row = Some(("codex", "claude"));
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let views = std::sync::Mutex::new(Vec::new());
    let seam = |layout: &crate::store::Layout| {
        views
            .lock()
            .unwrap()
            .push(crate::active_builds(layout).get("trust").copied());
    };
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.rustup_seam = &seam;
    let _ = take_markers_said();
    let code = install_toolset(&layout, &claude_and_codex_wanted(), &f, &seams);
    assert_eq!(code, 0);
    let mut at_payload = f.at_payload.lock().unwrap().clone();
    at_payload.sort();
    assert_eq!(
        at_payload
            .iter()
            .map(|(agent, set_in, _)| (agent.as_str(), *set_in))
            .collect::<Vec<_>>(),
        [("claude", true), ("codex", true)],
        "the agents' bytes wait for every archive of the set"
    );
    let markers = take_markers_said();
    let starts: Vec<&String> = markers
        .iter()
        .filter(|m| m.contains(NET_STARTING_MARKER))
        .collect();
    assert_eq!(starts.len(), 1, "one announcement: {markers:?}");
    assert!(
        starts[0].starts_with(&format!(
            "atpkg: {NET_STARTING_MARKER}installing 4 program(s) over the network: claude, \
             codex, tb, trust — {NET_OFF_SWITCH}"
        )),
        "the verb's own consent: {}",
        starts[0]
    );
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let rows = crate::status::read(&layout).unwrap().programs;
    assert_eq!(rows["claude"].state, "managed 2.1.281 — Anthropic latest");
    assert_eq!(rows["codex"].state, "managed 0.157.0 — OpenAI latest");
    assert_eq!(
        *views.lock().unwrap(),
        [Some(4821), Some(4821)],
        "the view follows trust at its flip, and the verb's end re-lays it"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent a script asked for waits for the toolchain's bytes, then goes first. A script's
/// `codex --version` (no terminal) bumps codex as a person's stub does, but nobody is waiting
/// at a terminal: codex's bytes wait for every archive of the set like any agent's, and once
/// the gate opens codex is the request taken first, ahead of claude's place in the table.
#[test]
fn a_script_bumped_agent_waits_for_the_toolchain_then_goes_first() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("beside-script-bump-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("beside-script-bump-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    crate::progress::append_bump(
        &layout,
        &crate::store::ToolName::new("codex").unwrap(),
        crate::progress::Asker::Script,
    )
    .unwrap();
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    let file = crate::progress::live_snapshot().expect("the pass is live");
    crate::progress::end_pass();
    assert_eq!(code, 0);
    let mut at_payload = f.at_payload.lock().unwrap().clone();
    at_payload.sort();
    assert_eq!(
        at_payload
            .iter()
            .map(|(agent, set_in, _)| (agent.as_str(), *set_in))
            .collect::<Vec<_>>(),
        [("claude", true), ("codex", true)],
        "codex, a script's request, waits for the set's archives like claude"
    );
    assert!(
        file.programs["codex"].bumped && !file.programs["claude"].bumped,
        "codex is taken as the request it is, first, and claude in its turn"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`write_toolchain_registry`] whose index publishes no channel this client follows, as a
/// machine with no channel for its triple reads it.
fn write_channelless_registry(dir: &Path) {
    write_toolchain_registry(dir);
    let body = std::fs::read_to_string(dir.join("index.toml"))
        .unwrap()
        .replace("name = \"stable\"", "name = \"elsewhere\"");
    std::fs::write(dir.join("index.toml"), body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("index.toml.sig"),
        sign(&RELEASE_SEED, body.as_bytes()),
    )
    .unwrap();
}

/// No channel here, and the promise goes with the plan that says so. A first run's set
/// completion finds no channel for this machine and plans nothing: the promise a stub waits
/// on goes then, so `targo` hears the truth at once rather than when the update exits. When
/// the agents the set was to carry land alone after it, their plan takes the promise over
/// (a stub waiting for claude never reads it gone before claude's row is up) and goes with
/// it.
#[test]
fn a_set_with_no_channel_hands_its_promise_to_the_plan_that_comes_next() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("no-channel-registry");
    write_channelless_registry(&dir);
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let pass = |layout: &crate::store::Layout,
                f: &SetBesideVendors,
                cfg: &crate::config::PackagesConfig,
                seams: &PassSeams<'_>| {
        crate::progress::promise_pass(
            layout,
            std::process::id(),
            crate::progress::SEED_PROMISE_SECS,
        );
        assert!(crate::progress::begin_pass(
            &layout.progress_file(),
            NET_PASS
        ));
        let code = run_update_pass(
            layout,
            cfg,
            f,
            &std::collections::BTreeMap::new(),
            true,
            seams,
        );
        crate::progress::end_pass();
        code
    };

    // The agents land alone: at each one's payload its plan is up and the promise gone.
    let layout = temp_layout("no-channel-agents");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    assert_eq!(pass(&layout, &f, &claude_and_codex_wanted(), &seams), 1);
    let owed = f.owed_at_payload.lock().unwrap().clone();
    assert_eq!(owed.len(), 2, "{owed:?}");
    for (agent, owed, rows) in &owed {
        assert!(!owed, "{agent}: the agents' plan took the promise with it");
        assert!(
            ["claude", "codex"]
                .iter()
                .all(|a| rows.iter().any(|r| r == a)),
            "{agent}: its plan is up: {rows:?}"
        );
    }
    assert!(
        crate::active_builds(&layout).contains_key("claude"),
        "and they land"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);

    // codex's read is still out when the set stops (its head answers only once claude's
    // payload is asked for): claude lands alone first, its plan keeping the promise for
    // codex, and codex's own plan, after it, takes the promise with it. claude's read is in
    // before the index is, so the plan tells the two apart with no grace to wait out.
    let layout = temp_layout("no-channel-late");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.head_waits_for = Some(("codex", "payload:claude"));
    f.resolve_waits_for = Some("read:claude");
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let read_in = |program: &str| f.note(format!("read:{program}"));
    let mut late = world_seams(&trust);
    late.path = Some(std::ffi::OsString::new());
    late.vendor_read_grace = std::time::Duration::ZERO;
    late.read_ahead_in = &read_in;
    assert_eq!(pass(&layout, &f, &claude_and_codex_wanted(), &late), 1);
    let owed = f.owed_at_payload.lock().unwrap().clone();
    assert_eq!(owed.len(), 2, "{owed:?}");
    let claude = owed.iter().find(|(agent, ..)| agent == "claude");
    assert!(
        claude.is_some_and(|(_, owed, rows)| *owed && !rows.iter().any(|r| r == "codex")),
        "claude's plan, without codex, kept the promise for it: {owed:?}"
    );
    let codex = owed.iter().find(|(agent, ..)| agent == "codex");
    assert!(
        codex.is_some_and(|(_, owed, rows)| !owed && rows.iter().any(|r| r == "codex")),
        "codex's own plan carries its row and took the promise: {owed:?}"
    );
    let active = crate::active_builds(&layout);
    assert!(
        active.contains_key("claude") && active.contains_key("codex"),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);

    // Nothing to land after it: the set completion's empty plan retires the promise.
    let layout = temp_layout("no-channel-alone");
    let f = SetBesideVendors::new(&dir, &layout, false);
    let none = crate::config::PackagesConfig {
        exclude: Some(
            crate::vendor_direct::VENDORS
                .iter()
                .map(|spec| spec.program.to_string())
                .collect(),
        ),
        ..Default::default()
    };
    assert_eq!(pass(&layout, &f, &none, &seams), 1);
    assert!(
        !crate::progress::pass_owed(&layout, now_secs()),
        "the set's plan of nothing retired the promise"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FIRST RUN'S VENDOR READS GO OUT WITH ITS INDEX RESOLVE. The resolve here answers only
/// once claude's documents have been asked for — what only a pass that reads the vendor
/// lane's heads and the documents they name beside its index resolve asks first (a pass
/// that reads them after the index never gets past the resolve). The plan then takes what
/// was read: each head asked once, and the agents land beside the set as ever.
#[test]
fn a_first_runs_vendor_reads_go_out_with_its_index_resolve() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("reads-ahead-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("reads-ahead-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.resolve_waits_for = Some("get:claude");
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let asked = f.asked.0.lock().unwrap().clone();
    for agent in ["claude", "codex"] {
        let head = format!("head:{agent}");
        assert_eq!(
            asked.iter().filter(|a| **a == head).count(),
            1,
            "{agent}'s head read once: {asked:?}"
        );
    }
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A VENDOR THAT DOES NOT ANSWER IS NOT THE SET'S ANNOUNCEMENT'S TO WAIT FOR. codex's head
/// read hangs until the set's first archive is asked for — a host that never answers, as a
/// name that does not resolve or a connect that never finishes, until long after: a pass
/// whose announcement waits for every vendor's plan never gets there. The set announces with
/// claude beside it and lands; codex, planned once its read is in, lands after the set,
/// announced and answered on its own; and the set's plan kept the promise for it. claude's
/// read is in before the index is, and codex's cannot be before the plan, so the plan tells
/// them apart with no grace to wait out.
#[test]
fn a_vendor_that_does_not_answer_lands_after_the_set_it_did_not_hold() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("late-vendor-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("late-vendor-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.head_waits_for = Some(("codex", "download:trust"));
    f.resolve_waits_for = Some("read:claude");
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let read_in = |program: &str| f.note(format!("read:{program}"));
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.vendor_read_grace = std::time::Duration::ZERO;
    seams.read_ahead_in = &read_in;
    let _ = take_markers_said();
    crate::progress::promise_pass(
        &layout,
        std::process::id(),
        crate::progress::SEED_PROMISE_SECS,
    );
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    crate::progress::end_pass();
    assert_eq!(code, 0);
    let markers = take_markers_said();
    let starts: Vec<&String> = markers
        .iter()
        .filter(|m| m.contains(NET_STARTING_MARKER))
        .collect();
    assert_eq!(starts.len(), 2, "{markers:?}");
    assert!(
        starts[0].contains("over the network: claude, tb, trust — "),
        "{}",
        starts[0]
    );
    assert!(
        starts[1].contains("over the network: codex — "),
        "{}",
        starts[1]
    );
    let owed = f.owed_at_payload.lock().unwrap().clone();
    let claude = owed.iter().find(|(agent, ..)| agent == "claude");
    assert!(
        claude.is_some_and(|(_, owed, rows)| *owed && !rows.iter().any(|r| r == "codex")),
        "the set's plan, without codex, kept the promise for it: {owed:?}"
    );
    let codex = owed.iter().find(|(agent, ..)| agent == "codex");
    assert!(
        codex.is_some_and(|(_, _, rows)| rows.iter().any(|r| r == "codex")),
        "codex's own plan carries its row: {owed:?}"
    );
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE SET'S MANIFESTS DO NOT WAIT FOR THE VENDOR PLAN. A first run's plan waits for a
/// vendor's read still out past the index, and codex's head here answers only once the set's
/// manifests have been asked for: a pass that asks for them only after its vendor plan is in
/// never gets there (the plan waits out a grace as long as the hang detector, and the head
/// read its hang detector). Asked beside the plan, they let codex's read in, and the set
/// announces once, with both agents beside it.
#[test]
fn a_first_run_asks_for_its_set_manifests_while_the_vendor_plan_waits() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("manifests-beside-plan-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("manifests-beside-plan-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.head_waits_for = Some(("codex", "prefetch:trust"));
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    seams.vendor_read_grace = aterm_spec::wait::MUST_HAPPEN;
    let _ = take_markers_said();
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let markers = take_markers_said();
    let starts: Vec<&String> = markers
        .iter()
        .filter(|m| m.contains(NET_STARTING_MARKER))
        .collect();
    assert_eq!(starts.len(), 1, "{markers:?}");
    assert!(
        starts[0].contains("over the network: claude, codex, tb, trust — "),
        "{}",
        starts[0]
    );
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FIRST RUN RECORDS ITS FLOOR WHILE ITS SET'S MANIFESTS ARE OUT: the floor's durable writes
/// stood between the last manifest and the announcement. Here the manifests arrive only once
/// the floor is recorded, so a pass that records it only when its set completion starts, after
/// them, never gets there (the prefetch's hang detector).
#[test]
fn a_first_run_records_its_floor_while_its_set_manifests_are_out() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("floor-beside-manifests-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("floor-beside-manifests-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.prefetch_waits_for_floor = true;
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    assert_eq!(crate::sig::Floor::new(layout.floor()).current(), 51);
    let active = crate::active_builds(&layout);
    assert!(
        ["trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN UPDATE'S FLIP IS NOT HELD BEHIND THE SET'S MANIFESTS. Only a first run asks for its
/// set's manifests beside its vendor plan, joined with it: on any other pass the apply lane
/// comes between the plan and the set completion. Here trust is live at an older build and
/// its tuple's tb absent, and the set's manifests are answered only once the apply lane has
/// asked for trust's new archive: a pass that waits for them before its apply lane never
/// gets there (the prefetch's hang detector).
#[test]
fn an_update_pass_does_not_hold_its_apply_lane_behind_the_sets_manifests() {
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("apply-before-set-manifests-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("apply-before-set-manifests-store");
    make_live(&layout, "trust", 4800);
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.prefetch_waits_for = Some("download:trust");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(agents_except(&[])),
        ..Default::default()
    };
    let trust = crate::vendor_direct::lane::world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &cfg,
        &f,
        &crate::active_builds(&layout),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let live = crate::active_builds(&layout);
    assert_eq!(
        (live.get("trust").copied(), live.get("tb").copied()),
        (Some(4821), Some(6)),
        "the tuple lands whole"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE DROPPED TRANSFER DOES NOT COST A FIRST RUN ITS TOOLCHAIN. The download-ahead lane's
/// download of trust's archive is cut short: the lane goes on to the set's other archives,
/// the pass fetches trust's itself when it reaches the tuple, and the whole set and both
/// agents land in this pass — where a lane whose first failure halted it, and stood in for
/// the pass's own download, aborted the toolchain's tuple.
#[test]
fn a_first_run_lands_its_set_whole_when_the_lane_loses_one_archive() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("lane-lost-one-registry");
    write_toolchain_registry_and(&dir, &[("ay", 18)]);
    let layout = temp_layout("lane-lost-one-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.set = &["ay", "tb", "trust"];
    *f.flaky.lock().unwrap() = vec!["trust"];
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let active = crate::active_builds(&layout);
    assert!(
        ["ay", "trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let downloads = f.downloads.lock().unwrap().clone();
    let by = |program: &str| -> Vec<String> {
        downloads
            .iter()
            .filter(|(p, _)| p == program)
            .map(|(_, thread)| thread.clone())
            .collect()
    };
    let lane = "atpkg-download-ahead";
    let trust_by = by("trust");
    assert_eq!(trust_by.len(), 2, "{downloads:?}");
    assert_eq!(trust_by[0], lane);
    assert_ne!(trust_by[1], lane, "the pass fetched it itself");
    for member in ["ay", "tb"] {
        assert_eq!(
            by(member),
            [lane],
            "{member}: the lane went on past the lost archive: {downloads:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A set unit the download-ahead lane does not fetch keeps the gate shut on the lane's
/// word: `ay`'s row carries no compressed size, so the lane leaves it to the pass, which
/// fetches it inline; the lane's last archive in is then not the set's, and the agents'
/// gate waits for the pass's own rule (no tuple left to land): every agent's bytes move
/// after trust flips.
#[test]
fn a_unit_the_lane_does_not_fetch_keeps_the_agents_behind_the_toolchain() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("beside-unfetched-registry");
    write_toolchain_registry_and(&dir, &[("ay", 18)]);
    let manifest = dir.join("pkg-ay-18.toml");
    let body = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("size = 100", "size = 0");
    std::fs::write(&manifest, body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("pkg-ay-18.toml.sig"),
        sign(&RELEASE_SEED, body.as_bytes()),
    )
    .unwrap();
    let layout = temp_layout("beside-unfetched-store");
    let mut f = SetBesideVendors::new(&dir, &layout, false);
    f.set = &["ay", "tb", "trust"];
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 0);
    let at_payload = f.at_payload.lock().unwrap().clone();
    assert_eq!(at_payload.len(), 2, "{at_payload:?}");
    assert!(
        at_payload.iter().all(|(_, _, trust_live)| *trust_live),
        "the agents' bytes wait for trust's flip: {at_payload:?}"
    );
    let active = crate::active_builds(&layout);
    assert!(
        ["ay", "trust", "tb", "claude", "codex"]
            .iter()
            .all(|p| active.contains_key(*p)),
        "{active:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A refused set leaves its agents to land alone, and the promise with them. A volume that
/// cannot hold the set refuses it up front, so the agents it was to carry land after it on
/// their own; the set completion's empty plan keeps the promise for theirs (a stub waiting
/// for claude never reads it gone before claude's row is up), and the agents' plan retires
/// it.
#[test]
fn a_refused_set_keeps_the_promise_for_the_agents_it_leaves_behind() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let _slot = crate::progress::own_pass_slot();
    let dir = scratch("refused-set-registry");
    write_toolchain_registry(&dir);
    // trust's tree is larger than any volume: the set is refused before it moves a byte.
    let manifest = dir.join("pkg-trust-4821.toml");
    let body = std::fs::read_to_string(&manifest).unwrap().replace(
        "disk_installed = 1048576",
        "disk_installed = 1125899906842624",
    );
    std::fs::write(&manifest, body.as_bytes()).unwrap();
    std::fs::write(
        dir.join("pkg-trust-4821.toml.sig"),
        sign(&RELEASE_SEED, body.as_bytes()),
    )
    .unwrap();
    let layout = temp_layout("refused-set-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let cfg = claude_and_codex_wanted();
    let index = crate::resolve_verified_index(
        &f,
        &layout,
        &(seams.anchor)(&layout),
        build_floor(&layout),
        (seams.now)(),
    )
    .expect("the registry's index");
    let installed = std::collections::BTreeMap::new();
    let (mut vendor, fresh) = vendor_plan(
        &layout,
        &f,
        &cfg,
        &installed,
        VendorLanes {
            update_installed: true,
            complete_the_set: true,
        },
        Some(&index),
        &seams,
    );
    assert!(!fresh.pending.is_empty(), "the agents are fresh");
    let mut rider = VendorRider {
        fresh,
        trust: seams.trust,
        warm: seams.warm_agents,
        landed: None,
    };
    crate::progress::promise_pass(
        &layout,
        std::process::id(),
        crate::progress::SEED_PROMISE_SECS,
    );
    assert!(crate::progress::begin_pass(
        &layout.progress_file(),
        NET_PASS
    ));
    let flip = crate::quiet::FlipGate {
        policy: crate::quiet::FlipPolicy::Now,
        now: (seams.now)(),
        running: seams.running,
    };
    let net = install_default_set_with_path(
        &layout,
        &f,
        &(seams.anchor)(&layout),
        &cfg,
        Consent::Explicit,
        (seams.now)(),
        Some(&index),
        seams.path.as_deref(),
        &flip,
        &std::collections::BTreeSet::new(),
        SetCompanions {
            vendor: Some(&mut rider),
            toolchain_flipped: &|_| {},
            vendors_after: false,
        },
    );
    assert_eq!(net.failures, 1, "the set is refused");
    assert!(
        rider.landed.is_none(),
        "and its agents are left to land alone"
    );
    assert!(
        crate::progress::pass_owed(&layout, now_secs()),
        "the set's plan kept the promise for the agents' plan"
    );
    land_fresh_alone(
        &layout,
        &f,
        Consent::Explicit,
        &rider.fresh,
        &mut vendor,
        &seams,
        true,
    );
    crate::progress::end_pass();
    let owed = f.owed_at_payload.lock().unwrap().clone();
    assert!(
        !owed.is_empty()
            && owed.iter().all(|(_, owed, rows)| {
                !owed
                    && ["claude", "codex"]
                        .iter()
                        .all(|a| rows.iter().any(|r| r == a))
            }),
        "the agents' plan took the promise with it: {owed:?}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent that fails beside the set cancels nothing: the set lands whole, the terminal
/// names what arrived, the pass exits 1 for the agents, and each agent's row is its own
/// lane's failure, never the set's "installing" row it was painted with before its bytes
/// moved.
#[test]
fn an_agent_failing_beside_the_set_leaves_the_set_and_its_own_row() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("beside-fail-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("beside-fail-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    f.vendors.downloads_fail.set(true);
    crate::progress::append_bump(
        &layout,
        &crate::store::ToolName::new("codex").unwrap(),
        crate::progress::Asker::Person,
    )
    .unwrap();
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let _ = take_markers_said();
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(code, 1);
    let markers = take_markers_said();
    assert!(
        markers.contains(&format!("atpkg: {NET_INSTALLED_MARKER}tb, trust")),
        "{markers:?}"
    );
    let active = crate::active_builds(&layout);
    assert_eq!(
        (active.get("trust"), active.get("claude")),
        (Some(&4821), None)
    );
    let rows = crate::status::read(&layout).unwrap().programs;
    for agent in ["claude", "codex"] {
        assert!(
            rows[agent].state.starts_with("error: "),
            "{agent}: {}",
            rows[agent].state
        );
    }
    assert_eq!(
        crate::progress::read_bump(&layout),
        ["codex"],
        "a failed requested agent keeps its priority for the next pass"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_requested_agent_waiting_for_its_signature_keeps_its_priority() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    if !cfg!(target_os = "macos") {
        return;
    }
    let dir = scratch("beside-deferred-bump-registry");
    write_toolchain_registry(&dir);
    let layout = temp_layout("beside-deferred-bump-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    crate::progress::append_bump(
        &layout,
        &crate::store::ToolName::new("claude").unwrap(),
        crate::progress::Asker::Person,
    )
    .unwrap();
    let trust = world::trust_timing_out();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_eq!(
        code, 0,
        "a signature check that did not finish is no failure"
    );
    assert!(crate::active_builds(&layout).contains_key("trust"));
    assert!(!crate::active_builds(&layout).contains_key("claude"));
    assert_eq!(
        crate::progress::read_bump(&layout),
        ["claude"],
        "the requested agent is still pending"
    );
    let installed = crate::active_builds(&layout);
    assert_eq!(
        run_update_pass(
            &layout,
            &claude_and_codex_wanted(),
            &f,
            &installed,
            true,
            &seams,
        ),
        0
    );
    assert_eq!(
        crate::progress::read_bump(&layout),
        ["claude"],
        "the next pass keeps the pending request after the store is no longer empty"
    );
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    assert_eq!(
        run_update_pass(
            &layout,
            &claude_and_codex_wanted(),
            &f,
            &crate::active_builds(&layout),
            true,
            &seams,
        ),
        0
    );
    assert!(crate::active_builds(&layout).contains_key("claude"));
    assert!(
        crate::progress::read_bump(&layout).is_empty(),
        "the request is consumed when the signature check finishes and the agent lands"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no index, the agents still land (design §1.5): alone and first, under their own
/// announcement and terminal, while the set completion that follows can do nothing.
#[test]
fn a_failed_index_resolve_lands_the_agents_alone() {
    use crate::vendor_direct::lane::world::{self, native_exe};
    let dir = scratch("beside-no-index");
    let layout = temp_layout("beside-no-index-store");
    let f = SetBesideVendors::new(&dir, &layout, false);
    f.vendors
        .publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    f.vendors.publish_codex_default("0.157.0");
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    let _ = take_markers_said();
    let code = run_update_pass(
        &layout,
        &claude_and_codex_wanted(),
        &f,
        &std::collections::BTreeMap::new(),
        true,
        &seams,
    );
    assert_ne!(code, 0, "the index lane failed");
    let markers: Vec<String> = take_markers_said()
        .into_iter()
        .filter(|m| m.contains(NET_STARTING_MARKER) || m.contains(NET_INSTALLED_MARKER))
        .collect();
    assert_eq!(markers.len(), 2, "{markers:?}");
    assert!(
        markers[0].contains("installing 2 program(s) over the network: claude, codex — "),
        "{}",
        markers[0]
    );
    assert_eq!(
        markers[1],
        format!("atpkg: {NET_INSTALLED_MARKER}claude, codex")
    );
    let active = crate::active_builds(&layout);
    assert!(active.contains_key("claude") && active.contains_key("codex"));
    let _ = std::fs::remove_dir_all(&layout.prefix);
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

/// A window hint advances the signed, installed ay build before an unrelated vendor GET
/// can answer. Negative controls: without the hint, or with the signed build below the
/// hint, the vendor runs first (§1.5); with the hinted index refused, vendor work still
/// runs before the pass reports the index failure.
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

/// The stall this lane exists for: the vendors' heads move while the ALab index is stale,
/// or none verifies at all, and the pass's vendor lane installs and then upgrades both,
/// apart from the index lane, which still refuses every ALab program on the same fetcher.
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

/// The window's head-watch pass is the update pass's unattended lane for one installed
/// program; a person's `update <program>` is the explicit door. Only the person's installs
/// what is missing, fails on an unreached vendor, and forgets a refusal.
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
        // A signature check that did not finish: quiet for the window (no failure, no
        // "Package update failed") and nothing moved; a person's door says it did not
        // happen; the next window pass whose check finishes lands it.
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
/// unreached) asks for nothing. (Every other verdict, a refusal included, is
/// `only_an_installed_verdict_is_a_landing`'s.)
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

/// Only an install is a landing: a rollback re-activates a build that already ran here, a
/// tombstone runs nothing, and every other verdict moved nothing, so none is warmed.
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

/// An index re-pin of a vendor program never downgrades or fights: an index pin whose
/// version is behind the vendor's head fetches nothing and never flips the store back;
/// an installed pin build is a floor, kept until a head passes it. The fake's release lane
/// panics on the pin's artifact, so a pass that planned the pin would fail here (without
/// `apply::plan_groups`' vendor skip it does).
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

/// No index rule decides a vendor program: a fresh index pinning a yanked legacy claude
/// leaves the vendor build and its shims untouched through the channel apply, the
/// single-program planners refuse it by name, an install of it forks to the vendor lane
/// before the index resolve, and the index's yank is latched for the vendor lane.
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
    // test-signed 2.1.282 is the vendor lane's refusal, never an index decision.
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

/// Two hours after the world's clock: a latch verified at [`world_now`] is past its hour.
fn two_hours_on() -> i64 {
    world_now() + 2 * 60 * 60
}

/// Half an hour after [`two_hours_on`]: a latch verified then is still current.
fn two_and_a_half_hours_on() -> i64 {
    two_hours_on() + 30 * 60
}

/// A yank published since the last pass binds the head watch's door: the latch the last
/// verification left is over an hour old, so the `--head-watch` door verifies the index
/// first and skips the head the newer index yanks. Within the hour after that verification
/// a door decides on the latch without asking the index.
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

/// A yank published since the last pass binds a first launch's install: the consented
/// `install claude` a pending stub re-enters ([`install_vendor_door`]) and the flow's own
/// install fork find no latch, verify the index first, and never install the yanked
/// version.
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

/// The door's resolve never blocks it: with the latch a day old and no index that
/// verifies (none published, or only a stale one), `install claude`, the head watch's door
/// and a person's `update claude` each try the index, then decide on the latch (whose yank
/// still binds) and land every admissible head.
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

/// A pass whose index lane cannot run still settles what its vendor lane moved: with the
/// index stale across two vendor upgrades, each pass exits 1 and the store keeps the live
/// build and one rollback.
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
/// the program's row says so where doctor looks, the pass's outcome where Settings does. A head kept at for a reason that is
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
        status.outcome.starts_with("claude 2.1.282 refused: "),
        "the outcome Settings shows names the refusal, never \"up to date\": {}",
        status.outcome
    );
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

/// Tier-1 conformance of `AtpkgPassStamps` (aterm-spec): the real writer (a whole recorded
/// `update` pass over the world's fake channel) and the real readers (`Stamps::read`,
/// `pkg_check::healed`, `pass_ended_since_queued`) step for step against the model, each
/// record projected onto `succ`/`fail` in ten-second ticks past the world's `NOW`. The
/// stand-down reader is checked as an implication (an `ok` stand-down only behind a reached
/// pass) and shown to stand down `ok` at least once. Negative controls: PK-3, the `Buggy`
/// model's cache-served pass stamps a success the real record does not; PK-4, the
/// historical reader (`updated_at` after the last success ⇒ failed) calls the door's write
/// a failed pass where the real reader does not.
#[test]
fn the_pass_record_refines_the_derived_model() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use aterm_update_core::pkg_check::{self, Stamps};
    let model = aterm_models_atpkg::atpkg_pass_stamps_model();
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

    // 2. This lane's pass is refused (a rate limit): it runs on the §14 cache, exits 1
    //    and stamps a failure, never a success.
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

    // 4. A sibling's pass, refused the same way, served from the cache. PK-3's replay:
    //    the historical writer stamps a success here, healing this lane's ladder and
    //    standing the waiter down with nothing reached; the real record does not.
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

/// Offline is not success: a pass whose index came only from the §14 cache and whose
/// vendor channels did not answer exits `PASS_OFFLINE_EXIT` (not 0, 1 or 75) and leaves
/// `last_success_at` where it was. One that reached either is an ordinary pass; one with
/// no index at all and no vendor reached is offline too.
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

/// A host that answered is not an offline machine: with the index source refusing (a rate
/// limit, a renamed repo) and every vendor channel down, the pass runs on the cached index
/// and exits 1, its outcome naming the refusal, never the offline code the window retries
/// quietly. A vendor channel that answered with a status is a host that answered too.
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

/// A pass that failed only for its cached index is recorded as that, and a child standing
/// down behind it exits as it did: the listing refused while no member failed, so exit 1,
/// nothing on stderr, and [`crate::status::stale_index_only_end`] holds for that pass end;
/// a `--wait-lock` child that stands down behind it says its line on stdout and exits 1
/// ([`stand_down_exit`]), which the window reads as a stale index, not "Package update
/// failed". Controls: a clean pass, an offline one, and a cached-index pass that also
/// failed a member are not that.
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
    // Control (the Apple anchor exists on macOS only): the cached index and a member
    // refused, a failure the stand-down still says on stderr.
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

/// Tier 1 of the derived model `AtpkgFullPassRule`, the queue: over every state either arm
/// reaches with a child waiting behind a pass that has since ended (`TakeLockBehind`
/// enabled), [`pass_ended_since_queued`] stands down exactly when the fixed model's child
/// does (always), and the replaced success-only rule, the negative control, exactly when
/// `Buggy=1`'s does.
#[test]
fn a_queued_pass_stands_down_as_its_derived_model_does() {
    use crate::status::full_pass_rule_tier1::{AGE_SECS, reachable, realize, walk};
    use aterm_models_atpkg::atpkg_full_pass_rule_model;
    use aterm_spec::derive::Model;
    let fixed = atpkg_full_pass_rule_model();
    let buggy = aterm_spec::interp::with_buggy(&fixed, 1);
    // The replaced rule: stand down behind a success stamped since the wait began.
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

/// A stale cache is offline only when nothing answered: the §14 cache aged past its window
/// off the network is offline (retried soon, no failure shown); the same stale index served
/// by a host that answered, or with the host refusing, is a failure.
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

/// An unreached pass is offline only when every failure was the network's: a member lost
/// at its manifest fetch while nothing answers is offline; one that failed at a step that
/// needs no network (a row this client cannot stage) is a failure, whatever the network
/// did.
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

/// Held, as the CLI edge holds every verb that takes the store lock, an idle pass writes
/// `status.toml` once and the record is whole; unheld, every writer rewrites it.
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

/// "LATEST" NEEDS A CHECK BEHIND IT: a vendor that cannot be reached leaves the row
/// alone for a day after the last check that reached it; past that the row says since
/// when it was not reached. (The managed-current marker's `not checked` is
/// `the_managed_current_line_lists_vendor_programs_at_their_vendors_latest`'s.)
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
    // Reached again: latest again.
    f.offline.set(false);
    assert!(run(world_now_plus_a_day));
    assert_eq!(row(), "managed 2.1.281 — Anthropic latest");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The vendor doors sweep only their own programs: the window's head-watch door, a pass
/// whose index lane cannot run, and `install <vendor>` each land a new claude, and none
/// sweeps trust's `.part` resume state or carried archive, or reclaims another program's
/// superseded build.
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

/// `install <vendor>` records its row through the lane's own writer: an install over a
/// yanked build that rolls back to the retained one says so, never "latest".
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

/// SEVERAL ROWS, ONE WRITE: the rows a call changes reach the record together — each printed,
/// every other row kept — where a write apiece cost a first run's four agent rows four fsyncs,
/// renames and directory syncs before its announcement; and a call that changes nothing writes
/// nothing.
#[test]
fn rows_recorded_together_are_one_write_of_the_record() {
    let layout = temp_layout("rows-one-write");
    record_unbuilt(&layout, "ay", "kept", "up to date".into());
    let installing = crate::state::agent_installing();
    let before = crate::status::counted::writes(&layout);
    assert!(record_states_if_changed(
        &layout,
        ["claude", "codex", "agy"].map(|p| (p, installing.clone()))
    ));
    assert_eq!(
        crate::status::counted::writes(&layout) - before,
        1,
        "three rows, one write"
    );
    let record = crate::status::read(&layout).unwrap();
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(record.programs[agent].state, installing, "{agent}");
    }
    assert_eq!(record.programs["ay"].state, "kept");
    assert_eq!(record.outcome, "up to date", "the outcome rides through");
    assert!(!record_states_if_changed(
        &layout,
        ["claude", "codex"].map(|p| (p, installing.clone()))
    ));
    assert_eq!(
        crate::status::counted::writes(&layout) - before,
        1,
        "nothing changed, nothing written"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A vendor fresh-install refusal stays on its row through the same pass's set
/// completion: the agent-rows loop never paints it `agent program — installing`.
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

/// A yank binds the pass that first sees it: the update pass and the toolset verb verify
/// the index before their vendor lane runs, so a yank published since the last pass keeps
/// the yanked head out now; when no index verifies, the vendor lane still runs, on the
/// latch.
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

/// A tombstoned vendor program is revisited by an update pass on a machine that never
/// adopted the set, as the index lane revisits one: the shim view is silent for it, and
/// once the vendor ships an admissible build the pass installs it, even with nothing else
/// installed.
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
    let _slot = crate::progress::own_pass_slot();
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

/// A first landing whose signature check did not finish is no failure: on the
/// set-completion lane's fresh path (`update` with `complete_the_set`, `install
/// --default-set`) a codesign timeout is a Skipped row with no error, no failure counted
/// and no `net-failed:`; the `seed-done:` terminal names what waits, and the next pass
/// lands the kept archive without a download. Control: a real signer refusal on the same
/// path still fails its row and says `net-failed:`.
#[test]
fn a_fresh_landing_whose_signature_check_did_not_finish_is_no_failure() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    // The Apple anchor exists on macOS only: off it no codesign check runs at all.
    if !cfg!(target_os = "macos") {
        return;
    }
    let _slot = crate::progress::own_pass_slot();
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
        is(&said, SEED_DONE_MARKER),
        "the announcement is answered: {said:?}"
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
    // Control: a real refusal on the same fresh path is still a failure, said.
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
/// `seed-done:`; nothing planned → nothing.
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
    assert!(
        both.starts_with(&format!("atpkg: {SEED_DONE_MARKER}")),
        "every landing deferred: the positive terminal, never a failure: {both}"
    );
}

/// THE TOOLSET VERB CHANGES ITS MIND BEFORE ITS PASS. The pass narrows what it wants by
/// the removal markers, so clearing them AFTER it honoured the old mind for exactly the pass
/// asked to change it: a legacy `trust` removal kept its group out of the explicit toolset
/// install, which then called the set complete (2026-08-27, live). A member removed by
/// `uninstall` and a recorded decline: the verb installs the member, and neither marker
/// survives it.
#[test]
fn the_toolset_verb_changes_its_mind_before_its_pass() {
    use crate::vendor_direct::lane::world::{self, Fake};
    let layout = world::layout("toolset-change-of-mind");
    let f = Fake::default();
    *f.index.borrow_mut() = Some(world::signed_index(44, "2099-01-01T00:00:00Z", &[]));
    f.publish_codex_default("0.157.0");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ay".into()]),
        ..Default::default()
    };
    let trust = world::trust();
    let seams = world_seams(&trust);
    record_removed(&layout, "codex");
    record_decline(&layout);
    assert!(
        removed_programs(&layout).contains("codex") && declined(&layout),
        "fixture: the old mind is on disk"
    );
    install_toolset(&layout, &cfg, &f, &seams);
    assert!(
        crate::active_builds(&layout).contains_key("codex"),
        "the removed member is installed by the verb asked to restore the set"
    );
    assert!(
        removed_programs(&layout).is_empty(),
        "the removals are gone"
    );
    assert!(!declined(&layout), "the decline is gone");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The explicit toolset verb over a vendor that did not serve its head exits 1 naming it,
/// never "already complete", and still adopts the set its index lane completed.
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

/// No index rule decides a vendor program through the set lane or the explicit door: an
/// index `system = "claude"` with a claude on PATH retires nothing, and `install claude`
/// asks the index nothing before its own lane.
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

/// THE VENDOR LANE NEVER WRITES THE RECORD for refused names (design S13): its landings run
/// on rider threads beside the pass thread, which writes the program rows, and an unlocked
/// rewrite there could lose one. Its `exposes` is the compiled table, which a shim may carry
/// whole (`vendor_direct::table`'s test), so it has nothing to record. Measured with a
/// record that a refused-names write would rewrite: an entry whose build left the store.
#[cfg(unix)]
#[test]
fn a_vendor_landing_writes_no_refused_names_record() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("vendor-no-refused-write");
    std::fs::create_dir_all(layout.build_dir("ay", 18)).unwrap();
    crate::status::record_refused_shims(&layout, "ay", 18, &["pwsh".to_string()]).unwrap();
    std::fs::remove_dir_all(layout.build_dir("ay", 18)).unwrap();
    let f = Fake::default();
    f.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
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
        download_turn: None,
    };
    let before = crate::status::counted::writes(&layout);
    let outcome = lane::install_one(&lane, spec, false);
    assert!(
        matches!(outcome.verdict, Verdict::Installed { .. }),
        "{:?}",
        outcome.verdict
    );
    assert_eq!(
        crate::status::counted::writes(&layout),
        before,
        "the lane wrote status.toml"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `atpkg rollback <vendor program>` builds no index fetcher: it moves to the newest
/// retained vendor build no signed yank names, shims the table's exposes and records a row
/// with the build's own root (so `atpkg verify` passes); with nothing retained, or only a
/// yanked build, it refuses and nothing moves.
#[test]
fn a_vendor_rollback_moves_to_the_retained_build_without_the_index() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("rollback-by-hand");
    let index = CountingFetcher::new(layout.prefix.join("unused-index"));
    let absent = crate::flow::rollback(
        &index,
        &layout,
        &crate::sig::testkit::anchor(),
        "stable",
        "ay",
        &[],
        vendor_floor(),
        world_now(),
    )
    .unwrap_err();
    assert!(matches!(absent, crate::FlowError::Rollback(_)), "{absent}");
    assert_eq!(
        index.resolves(),
        0,
        "the flow refuses before any index read"
    );
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
            download_turn: None,
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
            download_turn: None,
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

/// A vendor program its vendor serves this machine nothing for carries the canonical
/// `unavailable on <target>: <hint>` row — the state `doctor` reads as no fault — recorded
/// once, with no build, and never a failure of the pass.
#[test]
fn an_unserved_vendor_program_records_the_unavailable_row() {
    use crate::vendor_direct::lane::world::{self, Fake};
    use crate::vendor_direct::lane::{self, Verdict};
    use crate::vendor_direct::policy::Policy;
    let layout = world::layout("unserved-row");
    let f = Fake::default();
    let trust = world::trust();
    let none = Policy::default();
    let triple = "x86_64-unknown-linux-gnu";
    let lane = lane::Lane {
        layout: &layout,
        fetcher: &f,
        triple,
        policy: &none,
        trust: &trust,
        legacy_index: None,
        asked: false,
        now: world::NOW,
        download_turn: None,
    };
    let spec = crate::vendor_direct::spec("muse").unwrap();
    let outcome = lane::install_one(&lane, spec, false);
    let Verdict::Unavailable { state } = &outcome.verdict else {
        panic!("{outcome:?}");
    };
    let mut pass = VendorPass::default();
    report_vendor(&layout, &outcome, &mut pass, false, world::NOW);
    assert_eq!((pass.failures, pass.reached), (0, false));
    let row = crate::status::read(&layout).unwrap().programs["muse"].clone();
    assert_eq!(row.state, *state);
    assert!(
        row.state
            .starts_with(&format!("{}{triple}: ", crate::state::UNAVAILABLE_PREFIX)),
        "{}",
        row.state
    );
    assert_eq!(row.installed_build, None);
    assert!(f.gets.borrow().is_empty());
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `install muse` — and `update muse` while it is not installed — where Meta serves
/// nothing: the door asks no index and no host, and answers the lane's state whole, the
/// canonical non-error state the verb prints on stdout with a clean exit (as the index
/// lane's `NoArtifact`) and records as the row.
#[test]
fn an_unserved_vendor_install_answers_the_unavailable_state() {
    use crate::vendor_direct::lane::{Verdict, world};
    let layout = world::layout("unserved-install");
    let f = world::Fake::default();
    let req = crate::InstallRequest {
        channel: "stable",
        program: "muse",
        triple: "x86_64-unknown-linux-gnu",
        installed: None,
    };
    let outcome = crate::flow::vendor_install_outcome(
        &f,
        &layout,
        &world_anchor(&layout),
        build_floor(&layout),
        &req,
        crate::vendor_direct::spec("muse").unwrap(),
        &world::trust(),
        world::NOW,
    );
    assert_eq!((f.gets.borrow().len(), f.index_reads.get()), (0, 0));
    let Verdict::Unavailable { state } = outcome.verdict.clone() else {
        panic!("{outcome:?}");
    };
    let e = outcome.into_install(&layout).unwrap_err();
    assert!(matches!(e, crate::FlowError::VendorUnavailable(_)), "{e:?}");
    assert_eq!(canonical_non_error_state(&e), Some(state.clone()));
    assert_eq!(failed_install_state(&e), Some(state));
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
        download_turn: None,
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
    let table: Vec<String> = spec.shim_env.iter().map(|e| (*e).to_string()).collect();
    assert_eq!(
        crate::platform::shim_env_of(&shim),
        crate::shim_env::ShimEnv::admit(&table).unwrap(),
        "the table's env, never the index manifest's"
    );
    let row = crate::status::read(&layout).unwrap().programs["claude"].clone();
    assert_eq!(row.installed_build, Some(2_026_091_901));
    assert_eq!(row.state, "managed 2026091901 — rolled back from 2.1.281");
    assert!(row.tree_root.is_empty(), "no root was ever recorded for it");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// A ROLLBACK TO THE LEGACY BUILD IS ATTESTED, with no index asked: the first vendor
/// install overwrites the row that carried the legacy build's signed root, so the root is
/// kept in the program's stamp — from that row as it is replaced, or from the signed
/// manifest the lane read the build's version from — and the rollback's row carries it,
/// so `atpkg verify` matches instead of saying "no signed tree_root". A rollback row an
/// earlier client recorded without it is attested all the same, and the next pass that
/// keeps the build writes the root into it. (A root nothing holds, read again from the
/// build's signed manifest, is `vendor_direct::lane`'s
/// `a_legacy_root_nothing_holds_is_read_from_its_signed_manifest_once`.)
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
            download_turn: None,
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
}

/// A PROGRAM BUILT INTO aterm IS ANSWERED AS SUCH (amacs design §8.5). An `amacs` on
/// PATH outside the prefix is what the store's arms would call `system: … — not managed by
/// aterm`; `which` answers in the built-in state instead, naming the alias beside the
/// running aterm, or the other copy that runs ahead of it.
#[test]
fn which_amacs_is_built_in() {
    let l = temp_layout("which-built-in");
    let amacs = crate::builtin::builtin("amacs").unwrap();
    let version = aterm_types::version::app_version();
    let alias_dir = crate::builtin::alias_dir();
    let path = empty_path(&l);
    assert_eq!(
        which_line_in(&l, "amacs", Some(&path), crate::reroute::StubEnv::default()),
        Ok(crate::builtin::which_line_at(
            amacs,
            alias_dir.as_deref(),
            None,
            version
        ))
    );
    // Another aterm's alias first on PATH, outside the prefix: built in, shadowed by it,
    // and never `system:`.
    let bundle = crate::testdir::dir("atpkg-built-in-bundle-");
    std::fs::write(bundle.join("aterm"), b"#!/bin/true\n").unwrap();
    std::fs::set_permissions(bundle.join("aterm"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("aterm", bundle.join("amacs")).unwrap();
    let line = which_line_in(
        &l,
        "amacs",
        Some(bundle.as_os_str()),
        crate::reroute::StubEnv::default(),
    )
    .unwrap();
    assert_eq!(
        line,
        crate::builtin::which_line_at(
            amacs,
            alias_dir.as_deref(),
            Some(&bundle.join("amacs")),
            version
        )
    );
    assert!(
        line.starts_with(&format!("amacs → {}", bundle.join("amacs").display())),
        "{line}"
    );
    assert!(!line.contains("system:"), "{line}");
}

/// `list`'s human form ends with the programs built into aterm (ahead of a closing
/// `next` line, which stays last); its piped form is the store's alone, byte for byte as
/// before, on an empty store and on a live one.
#[test]
fn list_human_shows_built_in_row_and_porcelain_is_unchanged() {
    let section = crate::builtin::list_human_lines(aterm_types::version::app_version());
    assert!(
        section
            .iter()
            .any(|row| row.trim_start().starts_with("amacs ")),
        "{section:?}"
    );
    let human = |store: &[&str]| -> Vec<String> {
        store
            .iter()
            .map(ToString::to_string)
            .chain(section.iter().cloned())
            .collect()
    };

    let l = temp_layout("list-built-in");
    assert_eq!(
        list_lines(&l, false).unwrap(),
        ["atpkg: no programs installed"]
    );
    assert_eq!(list_lines(&l, true).unwrap(), human(&[EMPTY_LIST_HUMAN]));

    make_live(&l, "ay", 18);
    assert_eq!(list_lines(&l, false).unwrap(), ["ay\t18\tlive"]);
    assert_eq!(
        list_lines(&l, true).unwrap(),
        human(&["atpkg: 1 program(s) — 1 live", "  ay  18  live"])
    );

    let closing = with_built_in_section(vec!["rows".to_string(), LIST_NEXT_DOCTOR.to_string()]);
    let mut expected = human(&["rows"]);
    expected.push(LIST_NEXT_DOCTOR.to_string());
    assert_eq!(closing, expected, "the one next act stays the last line");
}

/// `install amacs` has nothing to fetch: it exits 0 at the dispatch edge, before the store
/// lock (`update amacs` likewise).
#[test]
fn install_amacs_is_a_noop_exit_0() {
    let run = |argv: &[&str]| main_entry(argv.iter().map(std::ffi::OsString::from).collect());
    assert_eq!(run(&["install", "amacs"]), ExitCode::SUCCESS);
    assert_eq!(run(&["update", "amacs"]), ExitCode::SUCCESS);
}

/// Each store verb's answer for a built-in name reads that verb's own flags off the table,
/// as the arity gate does: `update amacs --retry` is `update amacs`, never a pass that asks
/// the index for amacs; `--vendor`/`--alab`, `pin`, `unpin` and `rollback` refuse; and any
/// other argv is left to the verb's own grammar.
#[test]
fn built_in_answers_read_the_verbs_own_flags() {
    let argv = |xs: &[&str]| xs.iter().map(ToString::to_string).collect::<Vec<_>>();
    for (verb, xs, code) in [
        ("install", &["amacs"][..], ExitCode::SUCCESS),
        ("update", &["amacs", "--retry"], ExitCode::SUCCESS),
        ("update", &["--retry", "amacs"], ExitCode::SUCCESS),
        ("update", &["amacs", "--vendor"], ExitCode::from(1)),
        ("update", &["--alab", "amacs"], ExitCode::from(1)),
        ("verify", &["amacs"], ExitCode::SUCCESS),
        ("pin", &["amacs"], ExitCode::from(1)),
        ("unpin", &["amacs"], ExitCode::from(1)),
        ("rollback", &["amacs"], ExitCode::from(1)),
    ] {
        assert_eq!(
            built_in_answer(verb, &argv(xs)),
            Some(code),
            "{verb} {xs:?}"
        );
    }
    for (verb, xs) in [
        ("install", &["ay"][..]),
        ("install", &["--default-set", "amacs"]),
        ("update", &["--retry"]),
        ("run", &["amacs"]),
        ("remove", &["amacs"]),
    ] {
        assert_eq!(built_in_answer(verb, &argv(xs)), None, "{verb} {xs:?}");
    }
}

/// `uninstall amacs` refuses with exit 1: a built-in program cannot leave the binary, so
/// the refusal names the setting that ends its role instead. atpkg has no `remove`; the
/// word other package managers use is answered with `uninstall`.
#[test]
fn remove_amacs_refuses_naming_the_setting() {
    let run = |argv: &[&str]| main_entry(argv.iter().map(std::ffi::OsString::from).collect());
    let answer = crate::builtin::verb_answer(
        "uninstall",
        &["amacs".to_string()],
        &[],
        aterm_types::version::app_version(),
    )
    .unwrap();
    assert!(answer.refused);
    assert!(
        answer.line.contains("`[amacs] default_editor = false`"),
        "{}",
        answer.line
    );
    assert_eq!(run(&["uninstall", "amacs"]), ExitCode::from(1));
    assert_eq!(did_you_mean("remove"), Some("uninstall"));
    assert_eq!(did_you_mean("rm"), Some("uninstall"));
}

/// The `c-toolchain:` marker names what the window files (design init-system §4.2,
/// `base.clt`): `missing` where no toolchain is installed, `broken` where one cannot build
/// and an install fixes it (off macOS), `ready` wherever an install would fix nothing, no
/// line where nothing was probed; and a terminal shows none of it.
#[test]
fn the_c_toolchain_marker_files_what_an_install_fixes_and_is_ready_elsewhere() {
    use crate::prereq::CcVerdict;
    let cc = std::path::PathBuf::from("/usr/bin/cc");
    let said = |body: &str| Some(format!("atpkg: {C_TOOLCHAIN_MARKER}{body}"));
    let (missing, ready) = (said(C_TOOLCHAIN_MISSING), said(C_TOOLCHAIN_READY));
    assert_eq!(
        super::c_toolchain_marker(&CcVerdict::NoDeveloperDir),
        missing
    );
    assert_eq!(super::c_toolchain_marker(&CcVerdict::NoDriver), missing);
    assert_eq!(
        super::c_toolchain_marker(&CcVerdict::Ready { driver: cc.clone() }),
        ready
    );
    let broken = CcVerdict::Broken {
        driver: cc.clone(),
        why: "fatal error: stdio.h: No such file or directory".into(),
    };
    assert_eq!(
        super::c_toolchain_marker(&broken),
        if cfg!(target_os = "macos") {
            ready.clone()
        } else {
            said(C_TOOLCHAIN_BROKEN)
        },
        "a refusing compiler is filed only where an install fixes it, and never as missing"
    );
    assert_eq!(
        super::c_toolchain_marker(&CcVerdict::Unanswered { driver: cc.clone() }),
        ready
    );
    assert_eq!(
        super::c_toolchain_marker(&CcVerdict::Unproven {
            driver: cc,
            why: "no temp".into()
        }),
        ready
    );
    assert_eq!(super::c_toolchain_marker(&CcVerdict::NotProbed), None);
    assert_eq!(super::human_marker(missing.as_deref().unwrap()), None);
}

/// The seed's `c-toolchain:` line (design init-system §4.2), read without spawning
/// anything: on macOS the install shim with no developer directory behind it is `missing`
/// once the toolset is here to need the tools (a toolset that arrived over SSH is filed at
/// the next launch; a first launch leaves the filing to the pass that lays the set down),
/// and a developer directory holding a clang is `ready`, which ends the entry. Off macOS a
/// found `cc` may still lack headers, so the seed says nothing at all.
#[cfg(unix)]
#[test]
fn the_seed_reads_the_command_line_tools_without_running_anything() {
    let root = crate::testdir::dir("atpkg-seed-cc-");
    let shim = root.join("usr-bin");
    let ran = root.join("ran");
    for tool in ["cc", "xcrun"] {
        let path = shim.join(tool);
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    let developer = root.join("CommandLineTools");
    let host = crate::prereq::CcHost {
        shim_dir: Some(shim.clone()),
        xcode_select_link: root.join("no-link"),
        default_developer_dir: developer.clone(),
        bound: std::time::Duration::from_secs(60),
        tmp_root: root.to_path_buf(),
    };
    let path = Some(shim.as_os_str());
    let on_mac = |body: &str| {
        cfg!(target_os = "macos").then(|| format!("atpkg: {C_TOOLCHAIN_MARKER}{body}"))
    };
    assert_eq!(
        super::c_toolchain_seed_marker(path, &host, true),
        on_mac(C_TOOLCHAIN_MISSING),
        "no tools, a toolset here"
    );
    assert_eq!(
        super::c_toolchain_seed_marker(path, &host, false),
        None,
        "no tools, no toolset yet: the first pass files it"
    );
    let clang = developer.join("usr/bin/clang");
    std::fs::create_dir_all(clang.parent().unwrap()).unwrap();
    std::fs::write(&clang, "").unwrap();
    std::fs::set_permissions(&clang, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for toolset in [true, false] {
        assert_eq!(
            super::c_toolchain_seed_marker(path, &host, toolset),
            on_mac(C_TOOLCHAIN_READY),
            "toolset {toolset}"
        );
    }
    assert!(!ran.exists(), "nothing was spawned");
}

/// Only root holds a relay pass back (`sudo` keeps `$HOME`, and a root-owned rc would lock
/// its owner out of it); any other uid lays its own tree.
#[cfg(unix)]
#[test]
fn only_root_holds_a_relay_pass_back() {
    assert_eq!(
        super::relay_not_own_account_with(0),
        Some("it runs as root")
    );
    for euid in [1, 501, 65534] {
        assert_eq!(super::relay_not_own_account_with(euid), None, "{euid}");
    }
}
