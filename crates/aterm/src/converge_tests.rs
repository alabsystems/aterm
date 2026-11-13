// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `sync.identity` reads the fleet name beside the `aterm.toml` keys: a fleet name still due
/// makes the row due even where the keys are current, and the Mac's own names that differ
/// from the roster's label are recorded, made due only where the rule setup's one Mac
/// password left is in place to name them with no password (no pass asks a password of its
/// own). NEGATIVE CONTROLS: a fleet name in place leaves current keys current, a Mac already
/// called by its label records nothing about its own names, a person's own `[identity]
/// name` never names the Mac's own names, and without the rule a current read stays current.
#[test]
fn the_identity_row_reads_the_fleet_name_and_records_the_macs_own_name() {
    use aterm_agent::FleetName;
    let names = GeneratedNames::number_only(Number::parse("m42").unwrap());
    let keys = || IdentityRead::Current("label m42 in [ai] and [fleet] machine".to_owned());
    let named_as = |local_host: &str, computer: &str| OwnNames {
        computer: Some(computer.to_owned()),
        local_host: Some(local_host.to_owned()),
        host: Some(local_host.to_owned()),
    };
    let own = named_as("Andrews-MacBook-Pro", "Andrews-MacBook-Pro");
    let due = FleetName::Due {
        from: "node-3fa2".to_owned(),
    };
    match with_fleet_name(keys(), "m42", Some(&names), &due, &own) {
        IdentityRead::Due { from, to } => {
            assert_eq!(from, "fleet name node-3fa2");
            assert!(to.contains("fleet name m42"), "{to}");
            assert!(to.contains(own.local_host.as_deref().unwrap()), "{to}");
        }
        other => panic!("{other:?}"),
    }
    match with_fleet_name(keys(), "m42", Some(&names), &FleetName::Current, &own) {
        IdentityRead::Current(now) => {
            assert!(now.contains("fleet name m42"), "{now}");
            assert!(
                now.contains(
                    "this Mac's own name is Andrews-MacBook-Pro (Local hostname), not m42"
                ),
                "{now}"
            );
        }
        other => panic!("{other:?}"),
    }
    // A half-applied rename (one `[names]` row refused) or a Computer Name renamed in System
    // Settings still names the Mac otherwise, and so does a name scutil could not read.
    for half in [
        named_as("m42", "Andrew\u{2019}s MacBook Pro"),
        named_as("Andrews-MacBook-Pro", "m42"),
        OwnNames {
            computer: None,
            local_host: Some("m42".to_owned()),
            host: Some("m42".to_owned()),
        },
    ] {
        match with_fleet_name(keys(), "m42", Some(&names), &FleetName::Current, &half) {
            IdentityRead::Current(now) => {
                assert!(now.contains("own name is"), "{half:?}: {now}");
            }
            other => panic!("{other:?}"),
        }
    }
    let named = named_as("m42", "m42");
    match with_fleet_name(keys(), "m42", Some(&names), &FleetName::Current, &named) {
        IdentityRead::Current(now) => assert!(!now.contains("own name"), "{now}"),
        other => panic!("{other:?}"),
    }
    match with_fleet_name(keys(), "studio", Some(&names), &FleetName::Current, &own) {
        IdentityRead::Current(now) => assert!(!now.contains("own name"), "{now}"),
        other => panic!("{other:?}"),
    }
    let keys_due = IdentityRead::Due {
        from: "[ai] machine = unset".to_owned(),
        to: "label m42 in [ai] machine".to_owned(),
    };
    match with_fleet_name(keys_due, "m42", Some(&names), &due, &named) {
        IdentityRead::Due { from, .. } => {
            assert_eq!(from, "[ai] machine = unset, fleet name node-3fa2");
        }
        other => panic!("{other:?}"),
    }
    let taken = FleetName::Taken("node-77".to_owned());
    assert!(matches!(
        with_fleet_name(keys(), "m42", Some(&names), &taken, &named),
        IdentityRead::Current(_)
    ));
    let off = IdentityRead::NotApplicable("off: [identity] name = false".to_owned());
    assert_eq!(
        with_fleet_name(off.clone(), "m42", Some(&names), &due, &own),
        off
    );
    let current = with_fleet_name(keys(), "m42", Some(&names), &FleetName::Current, &own);
    match own_name_due(current.clone(), Some("m42"), true) {
        IdentityRead::Due { from, to } => {
            assert_eq!(from, "this Mac's own names");
            assert!(
                to.ends_with("own names m42, through setup's naming rule"),
                "{to}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(own_name_due(current.clone(), Some("m42"), false), current);
    assert_eq!(own_name_due(current.clone(), None, true), current);
    assert_eq!(own_name_due(off.clone(), Some("m42"), true), off);
}

#[test]
fn hostname_only_drift_is_visible_and_makes_the_identity_repair_due() {
    use aterm_agent::FleetName;
    let names86 = GeneratedNames::number_only(Number::parse("m86").unwrap());
    let names183 = GeneratedNames::number_only(Number::parse("m183").unwrap());
    let names184 = GeneratedNames::number_only(Number::parse("m184").unwrap());

    let keys = IdentityRead::Current("label m86 in [ai] and [fleet] machine".to_owned());
    for host in [Some("m184"), None] {
        let own = OwnNames {
            computer: Some("m86".to_owned()),
            local_host: Some("m86".to_owned()),
            host: host.map(str::to_owned),
        };
        let pending = own_name_pending("m86", Some(&names86), &own)
            .expect("HostName still needs repair while the other two names match");
        assert!(pending.contains("(Hostname)"), "{pending}");
        assert!(pending.contains(host.unwrap_or("unread")), "{pending}");
        let row = with_fleet_name(
            keys.clone(),
            "m86",
            Some(&names86),
            &FleetName::Current,
            &own,
        );
        assert!(matches!(&row, IdentityRead::Current(words) if words.contains(&pending)));
        assert!(matches!(
            own_name_due(row, Some("m86"), true),
            IdentityRead::Due { .. }
        ));
        assert_eq!(
            own_name_pending("studio", Some(&names86), &own),
            None,
            "a separate identity name still leaves OS naming alone"
        );
    }
    let repaired = OwnNames {
        computer: Some("m86".to_owned()),
        local_host: Some("m86".to_owned()),
        host: Some("m86".to_owned()),
    };
    assert_eq!(own_name_pending("m86", Some(&names86), &repaired), None);
    assert_eq!(
        own_name_due(keys.clone(), None, true),
        keys,
        "all three repaired names leave the current identity row current"
    );

    let mut aliased = OwnNames {
        computer: Some("m183".to_owned()),
        local_host: Some("m183-2".to_owned()),
        host: Some("m183".to_owned()),
    };
    assert!(
        own_name_pending("m183", Some(&names183), &aliased)
            .unwrap()
            .contains("collision")
    );
    assert!(
        !own_name_repairable("m183", Some(&names183), &aliased),
        "a Bonjour collision stays visible without retrying its setter"
    );
    aliased.local_host = Some("m183".to_owned());
    aliased.computer = Some("m183 (2)".to_owned());
    assert!(!own_name_repairable("m183", Some(&names183), &aliased));
    assert!(
        own_name_pending("m183", Some(&names183), &aliased)
            .unwrap()
            .contains("Computer")
    );
    aliased.computer = Some("m183 (02)".to_owned());
    assert!(own_name_repairable("m183", Some(&names183), &aliased));
    aliased.computer = Some("m183".to_owned());
    for name in ["m183-x", "m183-02", "m183-1", "m183-2-extra", "m184-2"] {
        aliased.local_host = Some(name.to_owned());
        assert!(
            own_name_pending("m183", Some(&names183), &aliased).is_some(),
            "{name} is not the accepted local projection of m183"
        );
    }
    aliased.computer = Some("m184".to_owned());
    aliased.host = Some("m184".to_owned());
    aliased.local_host = Some("m183-2".to_owned());
    assert!(
        own_name_pending("m184", Some(&names184), &aliased).is_some(),
        "an earlier number's alias cannot fulfill a new naming intent"
    );
}

/// `sync.awake` plans a start only where one keeps the Mac awake: never over a person's off
/// or without an installed app that has the verb (in `/Applications` or `~/Applications`),
/// and never for a mug that runs and asks macOS again itself. The installed app is not asked
/// about over an off or a running mug; a stopped mug's candidate that did not answer fails
/// the read for the next pass.
#[test]
fn the_awake_row_plans_a_start_only_where_one_helps() {
    use aterm_acaffeine::held::Held;
    use aterm_acaffeine::launchd::Installed;
    let stopped = || Held::Stopped {
        why: Some("the menu bar refused the mug".to_owned()),
    };
    let at = || {
        Installed::At(PathBuf::from(
            "/Users//a/Applications/aterm.app/Contents/MacOS/aterm",
        ))
    };
    let absent = || Installed::Absent("aterm.app is not installed".to_owned());
    let unasked = || -> Installed { panic!("no start: the installed app is not asked") };
    assert!(matches!(
        awake_of(at, stopped()),
        Ok(AwakeRead::Due(why)) if why.contains("the menu bar refused the mug")
    ));
    assert!(matches!(
        awake_of(unasked, Held::Held { pid: 7 }),
        Ok(AwakeRead::Current(_))
    ));
    assert!(matches!(
        awake_of(unasked, Held::Off),
        Ok(AwakeRead::NotApplicable(_))
    ));
    assert!(matches!(
        awake_of(unasked, Held::NotHere("a private root".to_owned())),
        Ok(AwakeRead::NotApplicable(_))
    ));
    assert!(matches!(
        awake_of(absent, stopped()),
        Ok(AwakeRead::NotApplicable(_))
    ));
    assert!(
        awake_of(
            || Installed::Unanswered("did not answer `help acaffeine`".to_owned()),
            stopped()
        )
        .is_err()
    );
    assert!(matches!(
        awake_of(unasked, Held::NotHeld { pid: 7, why: None }),
        Ok(AwakeRead::Asking(_))
    ));
    assert!(matches!(
        awake_of(
            unasked,
            Held::Paused {
                pid: 7,
                why: aterm_acaffeine::model::Pause::LidClosed,
                lingering: false
            }
        ),
        Ok(AwakeRead::Current(why)) if why.contains("lid is closed")
    ));
    assert!(
        awake_of(
            unasked,
            Held::Unread {
                pid: 7,
                why: "hung".to_owned()
            }
        )
        .is_err()
    );
}

/// `sync.requests` runs the custodian's pass whatever this Mac's own pass did, and says
/// both: a member pass that fails (a damaged ledger on the vault's admin Mac) still lets the
/// custodian decide for the lab. A copy or publication that failed in the custodian's pass
/// fails the pass, naming the error, as the publish's own error did.
/// NEGATIVE CONTROL: a pass that returns on the member pass's error never runs the
/// custodian's; one that reads only the counts calls a failed publication a pass that went
/// through.
#[test]
fn a_failing_member_pass_still_runs_the_custodians_and_a_failed_copy_fails_the_pass() {
    use aterm_db::credentials::{Custodian, Pass};
    let ran = std::cell::Cell::new(false);
    let lapsed = || {
        ran.set(true);
        Ok(Some(Custodian {
            retired: vec![([1; 16], aterm_secrets::need::Reason::Lapsed)],
            ..Custodian::default()
        }))
    };
    let said = requests_pass(|| Err("needs.toml: damaged".to_owned()), lapsed).unwrap_err();
    assert!(ran.get(), "{said}");
    assert!(
        said.contains("needs.toml: damaged") && said.contains("1 retired"),
        "{said}"
    );
    let failed = Custodian {
        failed: vec![([2; 16], "the vault's publication failed: refused".into())],
        ..Custodian::default()
    };
    let said = requests_pass(|| Ok(Pass::default()), || Ok(Some(failed))).unwrap_err();
    assert!(
        said.contains("1 failed") && said.contains("publication failed: refused"),
        "{said}"
    );
    assert_eq!(
        requests_pass(|| Ok(Pass::default()), || Ok(None)),
        Ok("0 posted, 0 answered, 0 decided here, 0 offered, 0 swept".to_owned())
    );
}

/// A need ledger `sync.requests` cannot read (damaged, on the vault's admin Mac) leaves the
/// pass due, saying so, so its apply runs and the custodian's half still decides for the
/// lab; the probe failing instead would read as unusable and run nothing.
/// NEGATIVE CONTROL: the same ledger on a Mac that is no Writer, or with Commons off, stays
/// idle, and a config that cannot be read still fails the read.
#[test]
fn a_need_ledger_that_cannot_be_read_still_runs_the_requests_pass() {
    let damaged = || Err("needs.toml: not a need ledger aterm wrote".to_owned());
    let due = requests_read(damaged(), || Ok(true), || true);
    assert!(
        matches!(&due, Ok(RequestsRead::Due(said)) if said.contains("needs.toml: not a need ledger")),
        "{due:?}"
    );
    assert!(matches!(
        requests_read(damaged(), || Ok(true), || false),
        Ok(RequestsRead::Idle(_))
    ));
    assert!(matches!(
        requests_read(damaged(), || Ok(false), || true),
        Ok(RequestsRead::Idle(_))
    ));
    assert!(requests_read(Ok(Vec::new()), || Err("aterm.toml".to_owned()), || true).is_err());
    assert_eq!(
        requests_read(Ok(Vec::new()), || Ok(true), || true),
        Ok(RequestsRead::Due(format!(
            "no needs on this {}",
            aterm_types::machine_name::OS_NOUN
        )))
    );
}

/// `[harness] accounts = "off"` turns the pass's accounts row off before anything else is
/// read; the same Mac without it reads its accounts (here: no source in a scratch home).
/// NEGATIVE CONTROL: a read that ignores the switch reads the off Mac as having no source.
#[test]
fn harness_accounts_off_turns_the_accounts_row_off() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (home, state) = (scratch.path().join("home"), scratch.path().join("state"));
    let config = scratch.path().join("aterm.toml");
    let roster = scratch.path().join("accounts.toml");
    aterm_uds::atomic_file::write(&config, b"[harness]\naccounts = \"off\"\n", 0o600).unwrap();
    assert_eq!(
        accounts_at(Some(&config), &home, &state, &roster, 1_800_000_000),
        AccountsRead::Off
    );
    std::fs::write(&config, "[harness]\naccounts = \"wall\"\n").unwrap();
    assert!(matches!(
        accounts_at(Some(&config), &home, &state, &roster, 1_800_000_000),
        AccountsRead::NoSource(_)
    ));
    assert!(
        !home.exists() && !state.exists() && !roster.exists(),
        "the read wrote nothing"
    );
}

#[test]
fn a_default_kimi_pool_is_a_converge_source_without_a_claude_source() {
    use aterm_agent::harness::{accounts, accounts_sync};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let (home, state, config_dir) = (
        scratch.path().join("home"),
        scratch.path().join("state"),
        scratch.path().join("config"),
    );
    let secrets = home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    let config = config_dir.join("aterm.toml");
    let roster = config_dir.join("accounts.toml");
    let configured = "[fleet]\nmachine = \"m16\"\n";
    aterm_uds::atomic_file::write(&config, configured.as_bytes(), 0o600).unwrap();
    std::fs::write(&roster, "").unwrap();
    let kimi = accounts::roster_beside_for(&config, accounts::Cli::Kimi).unwrap();
    let source = secrets.join(accounts_sync::cli_fleet_file(accounts::Cli::Kimi));
    let fleet = concat!(
        "[[account]]\nlabel = \"kimi-1\"\nemail = \"kimi-one@example.com\"\n",
        "machines = [\"m16\"]\nregion = \"global\"\n",
    );
    std::fs::write(&source, fleet).unwrap();
    let now = 1_800_000_000;
    assert!(
        !accounts_sync::has_init_accounts_source(&home, &state, &roster, now),
        "the fixture has no Claude source"
    );
    std::fs::write(&config, "[fleet]\nmachine = \"m22\"\n").unwrap();
    assert!(
        matches!(
            accounts_at(Some(&config), &home, &state, &roster, now),
            AccountsRead::NoSource(_)
        ),
        "a default pool does not admit an unlisted machine"
    );
    std::fs::write(&config, configured).unwrap();
    assert!(
        matches!(
            accounts_at(Some(&config), &home, &state, &roster, now),
            AccountsRead::Due(why) if why.contains("kimi")
        ),
        "a listed Kimi pool is due by default without a local roster"
    );

    std::fs::write(
        &config,
        format!("{configured}[harness]\naccounts = \"off\"\n"),
    )
    .unwrap();
    assert_eq!(
        accounts_at(Some(&config), &home, &state, &roster, now),
        AccountsRead::Off,
        "the explicit off switch still wins"
    );
    std::fs::write(&config, configured).unwrap();
    let read = accounts_at(Some(&config), &home, &state, &roster, now);
    assert_eq!(std::fs::read_to_string(&source).unwrap(), fleet);
    assert!(std::fs::read(&roster).unwrap().is_empty());
    assert!(!kimi.exists(), "the probe creates no local Kimi roster");
    assert!(!state.exists(), "the probes write no account state");
    assert!(matches!(&read, AccountsRead::Due(_)), "{read:?}");

    // Retained state without a roster records a pool this machine left.
    let store = accounts::Store::at_for(&state, accounts::Cli::Kimi, &home);
    let mut left = accounts::State::default();
    left.accounts
        .insert("kimi-1".into(), accounts::Account::default());
    store.save(&left).unwrap();
    let before = std::fs::read(store.state_file()).unwrap();
    assert!(
        matches!(
            accounts_at(Some(&config), &home, &state, &roster, now + 1),
            AccountsRead::NoSource(_)
        ),
        "the default does not rejoin a pool this machine left"
    );
    assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
    assert!(!kimi.exists(), "the left pool keeps its roster absent");
}

#[test]
fn a_default_codex_pool_reads_and_applies_without_either_accounts_roster() {
    use aterm_agent::harness::{accounts, accounts_sync};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let (home, state, config_dir) = (
        scratch.path().join("home"),
        scratch.path().join("state"),
        scratch.path().join("config"),
    );
    let secrets = home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    let config = config_dir.join("aterm.toml");
    let roster = config_dir.join("accounts.toml");
    let codex = accounts::roster_beside_for(&config, accounts::Cli::Codex).unwrap();
    let configured = "[fleet]\nmachine = \"m16\"\n";
    let source = secrets.join(accounts_sync::cli_fleet_file(accounts::Cli::Codex));
    let fleet = "[[account]]\nlabel='codex-1'\nemail='codex-one@example.test'\nmachines=['m16']\n";
    std::fs::write(&source, fleet).unwrap();
    aterm_uds::atomic_file::write(
        &config,
        format!("{configured}[harness]\naccounts='off'\n").as_bytes(),
        0o600,
    )
    .unwrap();
    let now = 1_800_000_000;
    assert_eq!(
        accounts_at(Some(&config), &home, &state, &roster, now),
        AccountsRead::Off
    );
    assert!(!roster.exists() && !codex.exists() && !state.exists());

    std::fs::write(&config, configured).unwrap();
    assert!(!accounts_sync::has_init_accounts_source(
        &home, &state, &roster, now
    ));
    let before = accounts_at(Some(&config), &home, &state, &roster, now);
    assert!(!roster.exists() && !codex.exists() && !state.exists());
    let applied = accounts_sync::resync_for_setup(&home, &state, &roster, now, false);
    let after = accounts_at(Some(&config), &home, &state, &roster, now + 1);
    assert!(
        applied.synced
            && applied.changed
            && applied.failed.is_none()
            && applied.cli_failed.is_empty(),
        "{applied:?}"
    );
    assert!(std::fs::read_to_string(&codex).unwrap().contains("codex-1"));
    assert!(!roster.exists() && !config_dir.join(accounts_sync::FLEET_ROSTER_FILE).exists());
    for cli in [accounts::Cli::Grok, accounts::Cli::Kimi] {
        assert!(!accounts::roster_beside_for(&config, cli).unwrap().exists());
    }
    assert_eq!(std::fs::read_to_string(source).unwrap(), fleet);
    assert!(
        matches!(&before, AccountsRead::Due(why) if why.contains("codex"))
            && matches!(after, AccountsRead::Current(_)),
        "before={before:?}; applied={applied:?}; after={after:?}"
    );
}

/// A fabric with no command reads as `unset` says: the person's off and a CI run are left
/// alone, a fabric never turned off is the pass's to turn on, where the program it names for
/// the broker stays (an `aterm` on `$PATH`, this process the installed app, or this process
/// the file the account's installed command resolves to, as an install script leaves it on
/// Linux, where a job's `$PATH` has no `~/.local/bin`); a build, a copy on a mounted volume or
/// an unread executable found on no `$PATH` leaves it, since it would name its own file. The
/// live port passes only its process's reads.
/// NEGATIVE CONTROLS: a read that ignores `CI` hands a CI run's pass a fabric to turn on; one
/// that ignores the program hands a worktree build's pass a broker job naming that build; one
/// that reads only `$PATH` leaves the installed app's fabric off; one that ignores the
/// declaration leaves a script-installed aterm's fabric off; one that takes any declaration
/// as enough hands a build's pass a broker job naming that build.
#[test]
fn a_fabric_never_turned_off_is_the_passs_outside_ci() {
    use aterm_link::enable::Unset;
    let never = || Some(Unset::Never("off: never turned off".to_owned()));
    let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let build = Path::new("/Users//x/aterm-wt.noindex/target/debug/aterm");
    let mounted = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    let store = Path::new("/home/x/.local/lib/aterm/bin/aterm");
    assert!(matches!(
        fabric_unset(never(), false, true, Some(build), None),
        FabricRead::Unset(_)
    ));
    assert!(matches!(
        fabric_unset(never(), false, false, Some(app), None),
        FabricRead::Unset(_)
    ));
    assert!(matches!(
        fabric_unset(never(), false, false, Some(store), Some(store)),
        FabricRead::Unset(_)
    ));
    assert!(matches!(
        fabric_unset(never(), true, true, Some(app), None),
        FabricRead::NotApplicable(why) if why.contains("CI")
    ));
    assert!(matches!(
        fabric_unset(never(), true, false, Some(store), Some(store)),
        FabricRead::NotApplicable(why) if why.contains("CI")
    ));
    for (exe, declared) in [
        (Some(build), None),
        (Some(mounted), None),
        (None, None),
        (Some(store), None),
        (Some(build), Some(store)),
        (None, Some(store)),
    ] {
        assert!(
            matches!(
                fabric_unset(never(), false, false, exe, declared),
                FabricRead::NotApplicable(why) if why.contains("not the installed app")
            ),
            "{exe:?} declared {declared:?}"
        );
    }
    assert!(matches!(
        fabric_unset(
            Some(Unset::PersonOff("off by your choice".to_owned())),
            false,
            true,
            Some(store),
            Some(store)
        ),
        FabricRead::NotApplicable(why) if why == "off by your choice"
    ));
    assert!(matches!(
        fabric_unset(None, false, false, None, None),
        FabricRead::Current(_)
    ));
}

/// A Mac no control record reaches founds its vault's Commons only where the read says due
/// and nothing waits (the LAN broker answers, or the vault is not on the LAN); a founding
/// held for a person (an old approver) waits whatever the broker says. A read that is not
/// this Mac's to found names why; a member's reads as the observation says.
/// NEGATIVE CONTROL: a read that skips `waits` founds while the broker is absent; one that
/// reads a held founding as not this Mac's leaves it out of the pass's log line.
#[test]
fn a_founding_waits_for_the_lan_broker_and_names_why_it_is_not_this_macs() {
    let unseen = || "no Commons control entry is visible (control-not-visible)".to_owned();
    let found = "vault alab is open and has no Commons control";
    let due = || Foundable::Due(found.to_owned());
    let free = |_: &str| None;
    let absent = |what: &str| Some(format!("the LAN broker is not installed, so {what} waits"));
    assert_eq!(
        unseen_read(unseen(), due(), free),
        SharedRead::Foundable(found.to_owned())
    );
    let read = unseen_read(unseen(), due(), absent);
    assert!(
        matches!(&read, SharedRead::Deferred(why)
            if why.starts_with(found) && why.contains("the founding waits")),
        "{read:?}"
    );
    let held = "vault alab: this Mac's approver (/x/aterm, aterm 0.142.0) shares no root";
    assert_eq!(
        unseen_read(unseen(), Foundable::Waiting(held.to_owned()), free),
        SharedRead::Deferred(held.to_owned())
    );
    assert_eq!(
        unseen_read(unseen(), Foundable::Waiting(held.to_owned()), absent),
        SharedRead::Deferred(held.to_owned())
    );
    for foundable in [Foundable::NotAdmin, Foundable::Founded] {
        assert_eq!(
            unseen_read(unseen(), foundable, free),
            SharedRead::Unavailable(unseen())
        );
    }
    assert_eq!(
        unseen_read(
            unseen(),
            Foundable::Elsewhere("vault alab: it names 2 admins".to_owned()),
            free
        ),
        SharedRead::Unavailable(format!("{}; vault alab: it names 2 admins", unseen()))
    );
    assert!(
        matches!(
            unseen_read(unseen(), Foundable::Busy, free),
            SharedRead::Unavailable(why) if why.starts_with(&unseen())
        ),
        "a busy read founds nothing and fails nothing"
    );
}

/// A state that stays busy past the wait reads busy, never unavailable (which a read-back
/// reads as a failure), and reads nothing else; any other unavailable reading but an unseen
/// control record reads unavailable, reading no founding.
/// NEGATIVE CONTROL: a busy observation read as unavailable fails the founding read back.
#[test]
fn a_busy_state_reads_busy_and_reads_nothing_else() {
    use aterm_secrets::commons::{Observation, Unavailable};
    let no_vault = || -> Result<Option<aterm_secrets::client::Config>, String> {
        panic!("this read reads no vault")
    };
    let never = || -> Result<Foundable, String> { panic!("this read reads no founding") };
    let no_wait = |_: &aterm_secrets::client::Config, _: &str| -> Option<String> {
        panic!("this read asks nothing a founding waits on")
    };
    let read = shared_read(
        Observation::Unavailable(Unavailable::Busy),
        no_vault,
        never,
        no_wait,
    )
    .unwrap();
    assert!(
        matches!(&read, SharedRead::Busy(why) if why.contains("(vault-busy)")),
        "{read:?}"
    );
    let unconfigured = shared_read(
        Observation::Unavailable(Unavailable::Unconfigured),
        no_vault,
        never,
        no_wait,
    )
    .unwrap();
    assert!(
        matches!(&unconfigured, SharedRead::Unavailable(why) if why.contains("(vault-unconfigured)")),
        "{unconfigured:?}"
    );
}

/// The apply's report names what it founded or finished; a founding that stands for a person
/// waits for them, and one that may pass is the next pass's error.
/// NEGATIVE CONTROL: a report that reads a standing refusal as passing promises it a retry.
#[test]
fn the_apply_says_what_it_founded_and_what_stands() {
    let done = |published: bool, approved: &[&str]| {
        Ok(aterm_secrets::client::CommonsInit {
            network: [7; 16],
            authority: [1; 32],
            genesis: [2; 32],
            eligible: 3,
            published,
            approved: approved.iter().map(|host| (*host).to_owned()).collect(),
        })
    };
    let said = |done| match found_said(done) {
        Ok(SharedDone::Done(said)) => said,
        other => panic!("{other:?}"),
    };
    let network = "07".repeat(16);
    assert_eq!(
        said(done(true, &[])),
        format!("founded Commons network {network}")
    );
    assert!(said(done(true, &["m1", "m2"])).ends_with(
        "approving m1, m2: this Mac as its founder, the rest by the vault's open policy"
    ));
    assert!(said(done(false, &[])).contains("founded already"));
    assert_eq!(
        found_said(Err(NotFounded::Stands("answered 403".to_owned()))),
        Ok(SharedDone::Waiting("answered 403".to_owned()))
    );
    assert_eq!(
        found_said(Err(NotFounded::Again("timed out".to_owned()))),
        Err("timed out".to_owned())
    );
}

/// Only a publication to a vault on the LAN (a `.local` or single-label name, a link-local
/// address or one on an attached subnet) waits for the LAN broker, which is read only then; a vault at a public
/// URL or at a path waits for nothing.
/// NEGATIVE CONTROL: a `publish_waits` that gates every URL defers the public vault.
#[test]
fn only_a_vault_on_the_lan_waits_for_the_lan_broker() {
    use aterm_secrets::transport::Source;
    let config = |source: Source| aterm_secrets::client::Config {
        source,
        mirrors: Vec::new(),
        name: "alab".to_owned(),
        signer: None,
        followed: Vec::new(),
        host: "m1".to_owned(),
    };
    for (source, lan) in [
        (
            Source::Url("http://alab-secrets.local:7484/v1/alab".into()),
            true,
        ),
        (Source::Url("http://m11:7484/v1/alab".into()), true),
        (
            Source::Url("http://169.254.10.20:7484/v1/alab".into()),
            true,
        ),
        (Source::Url("http://[fe80::1]:7484/v1/alab".into()), true),
        (Source::Url("https://vault.example/v1/alab".into()), false),
        (Source::Url("https://8.8.8.8/v1/alab".into()), false),
        (Source::Path("/Volumes/Keys/team.vault".into()), false),
    ] {
        let reads = std::cell::Cell::new(0);
        let waits = publish_waits(&config(source.clone()), "the founding", || {
            reads.set(reads.get() + 1);
            Some("the LAN broker is not installed".to_owned())
        });
        assert_eq!(waits.is_some(), lan, "{source:?}: {waits:?}");
        assert_eq!(reads.get(), u32::from(lan), "{source:?}");
        assert_eq!(
            publish_waits(&config(source.clone()), "the founding", || None),
            None,
            "{source:?}"
        );
    }
}

/// A held copy's subscriber pass hands off to the installed release app: the first one the
/// folders hold (the system folder first), never to the program that runs, and never from a
/// program that is an installed release itself (another copy of the app included).
/// NEGATIVE CONTROL: a lookup that does not ask whether its own program is an installed
/// release hands an installed app's pass to another installed app.
#[test]
fn a_held_copy_hands_off_to_the_installed_app_and_an_installed_app_to_none() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let app = |dir: &str| {
        let macos = root.join(dir).join("aterm.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("aterm"), b"").unwrap();
        macos.join("aterm")
    };
    let (system, user) = (root.join("Applications"), root.join("home/Applications"));
    let held = root.join("share/aterm-secrets/aterm");
    assert_eq!(
        release_elsewhere(Some(&held), &[system.clone(), user.clone()]),
        None,
        "no app"
    );

    let in_user = app("home/Applications");
    let in_system = app("Applications");
    let both = [system, user];
    assert_eq!(
        release_elsewhere(Some(&held), &both),
        Some(in_system.clone())
    );
    assert_eq!(release_elsewhere(None, &both), Some(in_system.clone()));
    assert_eq!(
        release_elsewhere(Some(&in_system), &both),
        None,
        "the app itself"
    );
    assert_eq!(
        release_elsewhere(Some(&in_user), &both),
        None,
        "another installed app"
    );
    let dev = root.join("dev/aterm (dev).app/Contents/MacOS/aterm");
    assert_eq!(release_elsewhere(Some(&dev), &both), Some(in_system));
}

/// The hand-off runs the installed app with the held copy's words, as its own responsible
/// process (so macOS attributes what it does to aterm.app), out of the background
/// band the held copy starts its other children in (so the pass it runs lifts the fleet
/// book's lock holder).
/// NEGATIVE CONTROLS: a hand-off that keeps the held copy responsible, or that stays in the
/// band, fails here.
#[test]
fn the_hand_off_runs_the_app_as_its_own_responsible_process() {
    let args = ["init", "--converge", "--trigger", "subscriber"].map(str::to_owned);
    let program = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let command = hand_off_command(program, &args);
    assert_eq!(command.get_program(), program.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()
    );
    assert!(command.get_disclaim_responsibility());
    assert!(
        command.leaves_background_band(),
        "the pass handed off lifts its book lock's holder"
    );
}

/// `sync.node`'s read and its apply both answer a process that is not a shell of the
/// account's own, as a test binary under its private state root is, with that reason and
/// nothing else: neither reaches the job `systems.alab.aterm-node` or its unit file under
/// `$HOME`, which is still the account's here.
/// NEGATIVE CONTROL: without the gate in `through_node_job`, both go on to `sync.node`'s own
/// gates and answer another way (this test root's state is in no fleet).
#[test]
fn sync_node_leaves_the_job_alone_outside_the_accounts_own_shell() {
    let why = aterm_types::dirs::not_own_account().expect("a test binary's private root");
    assert_eq!(
        Live::here().node(),
        Ok(NodeRead::NotApplicable(why.to_owned()))
    );
    assert_eq!(
        Live::here().ensure_node(),
        Ok(format!("left as it is: {why}"))
    );
}

/// A window runs where the control socket folder holds a live instance's socket: not a dead
/// one's, not a token alone, not the latest-instance link, and not this process's own.
/// NEGATIVE CONTROL: a read that skips the liveness check takes a dead instance's socket
/// for a window.
#[test]
fn a_window_runs_only_where_a_live_instance_holds_a_socket() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let (live, dead) = (4_242_u32, 4_243_u32);
    let alive = |pid: u32| pid == live;
    assert!(!window_in(dir, &alive), "an empty folder");
    assert!(!window_in(&dir.join("missing"), &alive), "no folder at all");
    for name in [
        aterm_types::control_socket::instance_sock_name(dead),
        aterm_types::control_socket::instance_token_name(live),
        aterm_types::control_socket::LATEST_SOCK_FILE.to_owned(),
        aterm_types::control_socket::instance_sock_name(std::process::id()),
    ] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    assert!(
        !window_in(dir, &|pid| pid == live || pid == std::process::id()),
        "a dead instance, a token, the link and this process are no window"
    );
    std::fs::write(
        dir.join(aterm_types::control_socket::instance_sock_name(live)),
        b"",
    )
    .unwrap();
    assert!(window_in(dir, &alive));
}

/// `sync.identity` writes the resolved swarm label into `[ai] machine` and `[fleet] machine`
/// while preserving unrelated settings, ignores `~/.aterm/machine.toml`, and leaves personal
/// vaults and `[swarm] enabled = false` untouched.
/// NEGATIVE CONTROL: reading `~/.aterm/machine.toml` would adopt `m22` onto a clone or
/// personal vault instead of using the swarm roster (or `NotApplicable`).
#[test]
fn sync_identity_writes_ai_and_fleet_machine_and_ignores_legacy_machine_toml() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir_all(home.join(".aterm")).unwrap();
    // Legacy file from F7: must never be read by sync.identity.
    std::fs::write(home.join(".aterm/machine.toml"), "id = \"m22\"\n").unwrap();
    let paths = aterm_secrets::client::Paths::for_home(&home);
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    aterm_uds::atomic_file::write(&config, b"[harness]\naccounts = \"wall\"\n", 0o600).unwrap();

    // No vault / personal vault without roster -> NotApplicable, despite ~/.aterm/machine.toml.
    assert!(matches!(
        identity_at(&config, &record, &paths, &["Mac-mini-M4.local".to_owned()]).unwrap(),
        IdentityRead::NotApplicable(_)
    ));

    // Roster-resolved label -> Due, then the apply -> Current, keeping [harness].
    let due = identity_owed(&config, &record, Some("m112")).unwrap().read;
    assert!(
        matches!(&due, IdentityRead::Due { from, to } if from.contains("unset") && to.contains("m112")),
        "{due:?}"
    );
    let said = write_identity_at(&config, &record, Some("m112")).unwrap();
    assert!(said.contains("m112"), "{said}");
    assert_eq!(
        identity_owed(&config, &record, Some("m112")).unwrap().read,
        IdentityRead::Current("label m112 in [ai] and [fleet] machine".to_owned())
    );
    let written = std::fs::read_to_string(&config).unwrap();
    let parsed: aterm_toml::Table = aterm_toml::from_str(&written).unwrap();
    assert_eq!(parsed["ai"]["machine"].as_str(), Some("m112"));
    assert_eq!(parsed["fleet"]["machine"].as_str(), Some("m112"));
    assert_eq!(parsed["harness"]["accounts"].as_str(), Some("wall"));

    // [swarm] enabled = false stands aside as NotApplicable.
    aterm_uds::atomic_file::write(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    assert_eq!(
        identity_at(&config, &record, &paths, &["m15.local".to_owned()]).unwrap(),
        IdentityRead::NotApplicable("off: [swarm] enabled = false".to_owned())
    );
}

/// The machine keys `config` holds after a pass, in `[ai]`, `[fleet]` order.
fn machines(config: &Path) -> [Option<String>; 2] {
    read_identity_config(config).unwrap().machines
}

#[test]
fn a_chosen_machine_number_drives_existing_identity_and_os_projections() {
    use aterm_agent::machine_name::file::{self, MachineName};
    use aterm_types::machine_name::{Class, Nickname, Number};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let mut name = MachineName::unnumbered(Nickname::parse("MissBunny").unwrap(), 1);
    name.num = Some(Number::parse("m86").unwrap());
    name.class = Class::Chosen;
    file::read_or_create(&state, || Ok(name)).unwrap();
    let mut choice = NameChoice::Roster;
    let label =
        projected_machine_label(Some(&state), &mut choice, Some("m184".to_owned())).unwrap();
    assert_eq!(label.as_deref(), Some("m86"));
    let config = scratch.path().join("aterm.toml");
    let record = state.join("converge/identity-written.toml");
    aterm_uds::atomic_file::write(
        &config,
        b"[ai]\nmachine = \"m184\"\n[fleet]\nmachine = \"m184\"\n",
        0o600,
    )
    .unwrap();
    // The roster's previous label still proves these unrecorded projections were ours.
    write_identity_after_read(
        &config,
        Some(&state),
        &record,
        label.as_deref(),
        &|word| word == "m184",
        || {},
    )
    .unwrap();
    assert_eq!(machines(&config), [Some("m86".into()), Some("m86".into())]);
    let (target, os_label) = identity_target_from(&NameChoice::Roster, label.clone()).unwrap();
    let generated = generated_os_names(Some(&state), &target, os_label.as_deref())
        .unwrap()
        .unwrap();
    assert_eq!(generated.computer_name(), "m86 MissBunny");
    assert_eq!(generated.host_name(), "m86-missbunny");
    let old = OwnNames {
        computer: Some("m184".into()),
        local_host: Some("m184".into()),
        host: Some("m184".into()),
    };
    assert!(own_name_pending(&target, Some(&generated), &old).is_some());
    let current = OwnNames {
        computer: Some(generated.computer_name().into()),
        local_host: Some(generated.host_name().into()),
        host: Some(generated.host_name().into()),
    };
    assert!(own_name_pending("m86", Some(&generated), &current).is_none());
    // A newer Rename supersedes the earlier identity string, including the runtime guard
    // an older installed build needed. Turning naming off remains a hold.
    file::replace(&state, |name| {
        name.num = Some(Number::parse("m87").unwrap());
        Ok(())
    })
    .unwrap();
    aterm_uds::atomic_file::write(
        &config,
        b"[identity]\nname = \"m86\"\n[ai]\nmachine = \"m86\"\n[fleet]\nmachine = \"m86\"\n",
        0o600,
    )
    .unwrap();
    let mut choice = NameChoice::Yours("m86".into());
    let label = projected_machine_label(Some(&state), &mut choice, Some("m184".into())).unwrap();
    let (target, os_label) = identity_target_from(&choice, label.clone()).unwrap();
    assert_eq!(target, "m87");
    assert_eq!(os_label.as_deref(), Some("m87"));
    let generated = generated_os_names(Some(&state), &target, os_label.as_deref())
        .unwrap()
        .unwrap();
    assert!(own_name_pending(&target, Some(&generated), &current).is_some());
    write_identity_after_read(
        &config,
        Some(&state),
        &record,
        label.as_deref(),
        &|_| false,
        || {},
    )
    .unwrap();
    assert_eq!(machines(&config), [Some("m87".into()), Some("m87".into())]);
    let mut off = NameChoice::Off;
    let label = projected_machine_label(Some(&state), &mut off, None).unwrap();
    assert!(identity_target_from(&off, label).is_none());

    // The post-init port reads the saved source after root returned, without
    // forcing Auto/Seeded records through the person's policy-writing publisher.
    for class in [Class::Auto, Class::Seeded, Class::Chosen] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        aterm_uds::private_file::create_private_dir(&state).unwrap();
        let mut saved = MachineName::unnumbered(Nickname::parse("dog").unwrap(), 1);
        saved.num = Number::new(85);
        saved.class = class;
        file::read_or_create(&state, || Ok(saved)).unwrap();
        let config = scratch.path().join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
        let before = std::fs::read(state.join(file::FILE)).unwrap();
        let sys = aterm_machines::port::Fake::root();
        assert!(
            synchronize_saved_name_with(&sys, &state, &config, &|| Ok(())).is_err(),
            "missing naming rule stays pending"
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("sudo") || call.starts_with("ask")),
            "sync never asks for authority"
        );
        sys.stat_of(
            aterm_machines::root_steps::ADMITTED_NAME_RULE,
            aterm_machines::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o440,
                dir: false,
                symlink: false,
                len: 1,
            },
        );
        let observed = std::sync::Arc::new(std::sync::Mutex::new([
            "old".to_owned(),
            "old".to_owned(),
            "old".to_owned(),
        ]));
        let sets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (values, writes) = (
            std::sync::Arc::clone(&observed),
            std::sync::Arc::clone(&sets),
        );
        sys.simulate(move |_, cmd| {
            use aterm_machines::port::Ran;
            let args: Vec<_> = cmd.args.iter().map(|arg| arg.to_str().unwrap()).collect();
            if cmd.program == Path::new(aterm_machines::items::names::SCUTIL)
                && args.first() == Some(&"--get")
            {
                let index = aterm_machines::root_steps::NAME_PREFS
                    .iter()
                    .position(|pref| Some(pref) == args.get(1))
                    .unwrap();
                return Some(Ran::ok(&values.lock().unwrap()[index]));
            }
            if cmd.program == Path::new(aterm_machines::root_steps::SUDO)
                && args.first() == Some(&"-n")
            {
                let index = aterm_machines::root_steps::NAME_PREFS
                    .iter()
                    .position(|pref| Some(pref) == args.get(3))
                    .unwrap();
                values.lock().unwrap()[index] = args[4].to_owned();
                writes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Some(Ran::ok(""));
            }
            None
        });
        assert!(
            synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
                .unwrap()
                .is_some()
        );
        assert_eq!(*observed.lock().unwrap(), ["m85 Dog", "m85-dog", "m85-dog"]);
        assert_eq!(sets.load(std::sync::atomic::Ordering::SeqCst), 3);
        synchronize_saved_name_with(&sys, &state, &config, &|| Ok(())).unwrap();
        assert_eq!(
            sets.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "already-current names have no setters"
        );
        observed.lock().unwrap()[1] = "m85-dog-2".into();
        assert!(synchronize_saved_name_with(&sys, &state, &config, &|| Ok(())).is_err());
        assert_eq!(
            sets.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "collision remains pending without rename"
        );
        assert_eq!(
            std::fs::read(&config).unwrap(),
            b"",
            "OS-only projection writes no policy"
        );
        assert_eq!(
            std::fs::read(state.join(file::FILE)).unwrap(),
            before,
            "no class promotion or canonical edit"
        );
        if class != Class::Chosen {
            aterm_uds::atomic_file::write(&config, b"[identity]\nname = \"studio\"\n", 0o600)
                .unwrap();
            assert!(
                synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
                    .unwrap()
                    .is_none(),
                "a separate explicit identity source admits no OS projection"
            );
            assert_eq!(sets.load(std::sync::atomic::Ordering::SeqCst), 3);
        }
        aterm_uds::atomic_file::write(&config, b"[identity]\nname = false\n", 0o600).unwrap();
        assert!(
            synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
                .unwrap()
                .is_none()
        );
        assert!(
            synchronize_saved_name_with(&sys, &state, &config, &|| Err("setup stopped".into()))
                .is_err()
        );
        assert_eq!(sets.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    // A fresh saved-intent sync never ensures a name or falls back to a roster number.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let sys = aterm_machines::port::Fake::root();
    assert!(
        synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
            .unwrap()
            .is_none()
    );
    assert!(!state.join(file::FILE).exists());
    let mut saved = MachineName::unnumbered(Nickname::parse("dog").unwrap(), 1);
    file::read_or_create(&state, || Ok(saved.clone())).unwrap();
    assert!(
        synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
            .unwrap()
            .is_none()
    );
    saved.num = Number::new(85);
    saved.migration = Some(file::Migration::Waiting);
    file::replace(&state, |held| {
        *held = saved.clone();
        Ok(())
    })
    .unwrap();
    assert!(
        synchronize_saved_name_with(&sys, &state, &config, &|| Ok(()))
            .unwrap()
            .is_none()
    );
    assert!(
        sys.calls().is_empty(),
        "unassigned sources make no OS calls"
    );

    // The CLI reuses the exact noninteractive console admission and global init fence.
    let mut sys = aterm_machines::port::Fake::root();
    assert!(name_sync_current(&sys, &|| aterm_machines::init::Holds::NONE).is_err());
    sys.euid = 501;
    sys.stat_of(
        "/dev/console",
        aterm_machines::port::Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys.terminal = false;
    assert!(
        name_sync_current(&sys, &|| aterm_machines::init::Holds::NONE).is_ok(),
        "no person or terminal is required"
    );
    let mut holds = aterm_machines::init::Holds::NONE;
    holds
        .off
        .set(aterm_types::macos_setup::Setting::Repositories, true);
    assert!(
        name_sync_current(&sys, &|| holds.clone()).is_ok(),
        "repository hold is independent"
    );
    holds.newer = true;
    assert!(name_sync_current(&sys, &|| holds.clone()).is_err());
    holds.newer = false;
    holds.stopped = Some("setup records are unread".into());
    assert!(name_sync_current(&sys, &|| holds.clone()).is_err());
    sys.remote_login = true;
    assert!(name_sync_current(&sys, &|| panic!("remote naming reads no setup records")).is_err());
    sys.remote_login = false;
    sys.graphic_session = None;
    assert!(name_sync_current(&sys, &|| panic!("unknown session reads no setup records")).is_err());
    sys.graphic_session = Some(true);
    sys.stop_now();
    assert!(name_sync_current(&sys, &|| panic!("Stop is checked before setup records")).is_err());
}

#[test]
fn automatic_machine_numbers_leave_roster_naming_in_charge() {
    use aterm_agent::machine_name::file::{self, MachineName};
    use aterm_types::machine_name::{Class, Nickname, Number};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let initial = projection_target_now(&config, Some(state), Some("m184".into()))
        .unwrap()
        .unwrap();
    let fallback = || Some("m184".to_owned());
    let mut choice = NameChoice::Roster;
    assert_eq!(
        projected_machine_label(None, &mut choice, fallback()).unwrap(),
        fallback()
    );
    assert_eq!(
        projected_machine_label(Some(state), &mut choice, fallback()).unwrap(),
        fallback()
    );
    let provisional = generated_os_names(Some(state), "m184", Some("m184"))
        .unwrap()
        .unwrap();
    assert_eq!(
        provisional,
        GeneratedNames::number_only(Number::parse("m184").unwrap())
    );
    let mut name = MachineName::unnumbered(Nickname::parse("Heron").unwrap(), 1);
    name.num = Some(Number::parse("m8").unwrap());
    file::read_or_create(state, || Ok(name)).unwrap();
    for class in [Class::Auto, Class::Seeded] {
        file::replace(state, |held| {
            held.class = class;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            projected_machine_label(Some(state), &mut choice, fallback()).unwrap(),
            fallback()
        );
        assert!(
            generated_os_names(Some(state), "m184", Some("m184")).is_err(),
            "a mismatched canonical record must not fall back to a bare roster number"
        );
        let generated = generated_os_names(Some(state), "m8", Some("m8"))
            .unwrap()
            .unwrap();
        assert_eq!(generated.computer_name(), "m8 Heron");
        assert_eq!(generated.host_name(), "m8-heron");
        assert!(own_name_repairable(
            "m8",
            Some(&generated),
            &OwnNames::default()
        ));
        let admitted = projection_target_now(&config, Some(state), Some("m8".into()))
            .unwrap()
            .unwrap();
        assert_eq!(admitted.names.as_ref(), Some(&generated));
        assert!(
            initial
                .check_current(&config, Some(state), Some("m8".into()))
                .is_err(),
            "arrival of the canonical automatic name retires the provisional projection"
        );
    }
    aterm_uds::private_file::write_private(state, file::FILE, b"unread canonical name").unwrap();
    assert!(generated_os_names(Some(state), "m184", Some("m184")).is_err());
}

#[test]
fn automatic_system_names_keep_their_source_current_after_each_read() {
    use aterm_agent::machine_name::file::{self, MachineName};
    use aterm_types::machine_name::{Class, Nickname, Number};

    #[derive(Clone, Copy)]
    enum Change {
        Config(&'static str),
        Number,
        UnreadName,
        NicknameAndUnrelated,
        Unrelated,
        None,
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    for (case, field, change, completed) in [
        (
            "identity-off",
            "ComputerName",
            Change::Config("[identity]\nname = false\n"),
            0,
        ),
        (
            "swarm-off",
            "LocalHostName",
            Change::Config("[swarm]\nenabled = false\n"),
            1,
        ),
        (
            "same-target-new-policy",
            "ComputerName",
            Change::Config(
                "[identity]\nname = \"m86\"\n[ai]\nmachine = \"m86\"\n[fleet]\nmachine = \"m86\"\n",
            ),
            0,
        ),
        (
            "new-machine-key",
            "HostName",
            Change::Config("[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"m86\"\n"),
            2,
        ),
        ("new-number", "ComputerName", Change::Number, 0),
        ("unread-name", "ComputerName", Change::UnreadName, 0),
        (
            "unread-policy",
            "ComputerName",
            Change::Config("[identity\n"),
            0,
        ),
        (
            "nickname-and-unrelated",
            "LocalHostName",
            Change::NicknameAndUnrelated,
            1,
        ),
        ("unrelated", "ComputerName", Change::Unrelated, 3),
        ("unchanged", "ComputerName", Change::None, 3),
    ] {
        let dir = scratch.path().join(case);
        aterm_uds::private_file::create_private_dir(&dir).unwrap();
        let state = dir.join("state");
        aterm_uds::private_file::create_private_dir(&state).unwrap();
        let mut name = MachineName::unnumbered(Nickname::parse("MissBunny").unwrap(), 1);
        name.num = Some(Number::parse("m86").unwrap());
        name.class = Class::Chosen;
        file::read_or_create(&state, || Ok(name)).unwrap();
        let config = dir.join("aterm.toml");
        let record = state.join("converge/identity-written.toml");
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
        write_identity_after_read(
            &config,
            Some(&state),
            &record,
            Some("m184"),
            &|_| true,
            || {},
        )
        .unwrap();
        let projection = projection_target_now(&config, Some(&state), Some("m184".into()))
            .unwrap()
            .unwrap();
        assert_eq!(projection.target.0, "m86");
        let sys = aterm_machines::port::Fake::root();
        sys.stat_of(
            aterm_machines::root_steps::ADMITTED_NAME_RULE,
            aterm_machines::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o440,
                dir: false,
                symlink: false,
                len: 1,
            },
        );
        let config_to_change = config.clone();
        let state_to_change = state.clone();
        let read = format!("/usr/sbin/scutil --get {field}");
        let changed_on = read.clone();
        sys.simulate(move |_, cmd| {
            let key = cmd.display();
            if key == changed_on {
                // The read returns only after the real source changes; every effect is fake.
                match change {
                    Change::Config(text) => {
                        aterm_uds::atomic_file::write(&config_to_change, text.as_bytes(), 0o600)
                            .unwrap();
                    }
                    Change::Number => {
                        file::replace(&state_to_change, |held| {
                            held.num = Some(Number::parse("m87").unwrap());
                            Ok(())
                        })
                        .unwrap();
                    }
                    Change::UnreadName => {
                        aterm_uds::private_file::write_private(
                            &state_to_change,
                            file::FILE,
                            b"unread name",
                        )
                        .unwrap();
                    }
                    Change::NicknameAndUnrelated => {
                        file::replace(&state_to_change, |held| {
                            held.nick = Nickname::parse("Heron").unwrap();
                            held.rev += 1;
                            Ok(())
                        })
                        .unwrap();
                        let mut text = std::fs::read_to_string(&config_to_change).unwrap();
                        text.push_str(
                            "\n[harness]\naccounts = \"wall\"\n[identity]\ngit = false\n",
                        );
                        aterm_uds::atomic_file::write(&config_to_change, text.as_bytes(), 0o600)
                            .unwrap();
                    }
                    Change::Unrelated => {
                        let mut text = std::fs::read_to_string(&config_to_change).unwrap();
                        text.push_str(
                            "\n[harness]\naccounts = \"wall\"\n[identity]\ngit = false\n",
                        );
                        aterm_uds::atomic_file::write(&config_to_change, text.as_bytes(), 0o600)
                            .unwrap();
                    }
                    Change::None => {}
                }
            }
            if key.starts_with("/usr/sbin/scutil --get ") {
                Some(aterm_machines::port::Ran::ok("m184\n"))
            } else if key.starts_with("/usr/bin/sudo -n /usr/sbin/scutil --set ") {
                Some(aterm_machines::port::Ran::ok(""))
            } else {
                None
            }
        });
        let current = || projection.check_current(&config, Some(&state), Some("m184".into()));
        let generated = projection.names.as_ref().unwrap();
        let said = own_name_by_rule(&sys, generated, &current);
        let calls = sys.calls();
        assert!(calls.contains(&read), "{case}: the source change must run");
        let writes: Vec<_> = calls
            .iter()
            .filter(|call| call.contains(" --set "))
            .cloned()
            .collect();
        let expected: Vec<_> = aterm_machines::root_steps::NAME_PREFS
            .into_iter()
            .take(completed)
            .map(|field| {
                let slot = aterm_types::machine_name::OsSlot::from_macos_preference(field).unwrap();
                aterm_machines::port::Cmd::new(aterm_machines::root_steps::SUDO)
                    .args([
                        "-n",
                        aterm_machines::items::names::SCUTIL,
                        "--set",
                        field,
                        generated.for_slot(slot).unwrap(),
                    ])
                    .display()
            })
            .collect();
        assert_eq!(
            writes, expected,
            "{case}: {said:?}; no write may outlive its naming source"
        );
    }
}

/// Design §3 principle 4, "the person's choices win": a machine key the person set (here
/// before aterm ever wrote one, so no record names it) is kept as they set it, on every pass;
/// only the unset key takes the roster's label, and the row reads settled, never a change.
/// NEGATIVE CONTROL: a pass that writes the label wherever it differs overwrites `studio`.
#[test]
fn sync_identity_keeps_a_machine_name_the_person_set() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    aterm_uds::atomic_file::write(&config, b"[ai]\nmachine = \"studio\"\n", 0o600).unwrap();
    assert_eq!(
        identity_owed(&config, &record, Some("m112")).unwrap().read,
        IdentityRead::Due {
            from: "[fleet] machine = unset".to_owned(),
            to: "label m112 in [fleet] machine; [ai] machine = studio kept as you set it"
                .to_owned(),
        }
    );
    write_identity_at(&config, &record, Some("m112")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("studio".to_owned()), Some("m112".to_owned())]
    );
    for _ in 0..2 {
        assert_eq!(
            identity_owed(&config, &record, Some("m112")).unwrap().read,
            IdentityRead::Current(
                "label m112; [ai] machine = studio kept as you set it".to_owned()
            )
        );
        let said = write_identity_at(&config, &record, Some("m112")).unwrap();
        assert!(said.starts_with("left as it is"), "{said}");
        assert_eq!(machines(&config)[0].as_deref(), Some("studio"));
    }
    // A person who changes aterm's own write later keeps theirs too.
    std::fs::write(
        &config,
        "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"desk\"\n",
    )
    .unwrap();
    write_identity_at(&config, &record, Some("m113")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("studio".to_owned()), Some("desk".to_owned())]
    );
}

/// The config snapshot that made a key owed must still stand when the edit lands: a
/// later person choice, an Off, or a file that appeared is not a fresh write baseline.
/// NEGATIVE CONTROL: reopening after the decision overwrites the person's choice and
/// records that overwrite as aterm's own.
#[test]
fn sync_identity_refuses_a_choice_changed_after_its_read() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ordinary = scratch.path().join("ordinary.toml");
    let ordinary_record = scratch.path().join("ordinary-written.toml");
    write_identity_at(&ordinary, &ordinary_record, Some("m112")).unwrap();
    assert_eq!(
        machines(&ordinary),
        [Some("m112".into()), Some("m112".into())]
    );
    assert_eq!(read_written(&ordinary_record).unwrap(), machines(&ordinary));

    let mut outcomes = Vec::new();
    for (name, present, owned, changed) in [
        ("unset-key", true, false, "[ai]\nmachine = \"studio\"\n"),
        ("own-key", false, true, "[fleet]\nmachine = \"desk\"\n"),
        (
            "identity-name",
            true,
            false,
            "[identity]\nname = \"studio\"\n",
        ),
        ("identity-off", true, false, "[identity]\nname = false\n"),
        ("swarm-off", true, false, "[swarm]\nenabled = false\n"),
        ("appeared", false, false, "[ai]\nmachine = \"new-choice\"\n"),
    ] {
        let dir = scratch.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let config = dir.join("aterm.toml");
        let record = dir.join("identity-written.toml");
        let backup = dir.join("aterm.toml.bak");
        if present {
            aterm_uds::atomic_file::write(&config, b"# the observed config\n", 0o600).unwrap();
        }
        if owned {
            write_identity_at(&config, &record, Some("m111")).unwrap();
            assert_eq!(
                machines(&config),
                [Some("m111".into()), Some("m111".into())]
            );
        }
        let record_before = std::fs::read(&record).ok();
        let backup_before = std::fs::read(&backup).ok();
        let mut reached = false;
        let result =
            write_identity_after_read(&config, None, &record, Some("m112"), &|_| true, || {
                reached = true;
                std::fs::write(&config, changed).unwrap();
            });
        assert!(
            reached,
            "{name}: the ordinary read must actually owe a write"
        );
        outcomes.push((
            name,
            result.is_err(),
            std::fs::read_to_string(&config).unwrap() == changed,
            std::fs::read(&record).ok() == record_before,
            std::fs::read(&backup).ok() == backup_before,
        ));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, refused, kept, record, backup)| *refused && *kept && *record && *backup),
        "a changed config must be refused before its choice, record or backup is written: {outcomes:?}"
    );
}

/// A key that still holds exactly what aterm last wrote is aterm's: when the roster's label
/// moves, the pass moves it too.
#[test]
fn sync_identity_moves_its_own_write_when_the_label_moves() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    write_identity_at(&config, &record, Some("m112")).unwrap();
    let due = identity_owed(&config, &record, Some("m113")).unwrap().read;
    assert!(
        matches!(&due, IdentityRead::Due { from, to }
            if from == "[ai] machine = m112, [fleet] machine = m112"
                && to == "label m113 in [ai] machine and [fleet] machine"),
        "{due:?}"
    );
    write_identity_at(&config, &record, Some("m113")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("m113".to_owned()), Some("m113".to_owned())]
    );
}

/// `[identity] name = false` turns the row off: nothing is read as due and nothing written.
#[test]
fn sync_identity_is_off_where_identity_name_is_false() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let paths = aterm_secrets::client::Paths::for_home(&home);
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    aterm_uds::atomic_file::write(&config, b"[identity]\nname = false\n", 0o600).unwrap();
    let off = IdentityRead::NotApplicable("off: [identity] name = false".to_owned());
    assert_eq!(
        identity_at(&config, &record, &paths, &["m15.local".to_owned()]).unwrap(),
        off
    );
    assert_eq!(
        identity_owed(&config, &record, Some("m112")).unwrap().read,
        off
    );
    let said = write_identity_at(&config, &record, Some("m112")).unwrap();
    assert_eq!(said, "left as it is: off: [identity] name = false");
    assert_eq!(machines(&config), [None, None]);
    assert!(!record.exists());
}

/// `[identity] name = "<name>"` is the person's name for this Mac and wins over the roster's
/// label; `[identity] git = false` is said plainly (no pass writes git's identity).
#[test]
fn sync_identity_writes_the_persons_identity_name_over_the_roster_label() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    aterm_uds::atomic_file::write(
        &config,
        b"[identity]\nname = \"studio\"\ngit = false\n",
        0o600,
    )
    .unwrap();
    let due = identity_owed(&config, &record, Some("m112")).unwrap().read;
    assert!(
        matches!(&due, IdentityRead::Due { to, .. }
            if to.starts_with("your name studio in [ai] machine and [fleet] machine")
                && to.contains("[identity] git = false")),
        "{due:?}"
    );
    write_identity_at(&config, &record, Some("m112")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("studio".to_owned()), Some("studio".to_owned())]
    );
    assert_eq!(
        identity_owed(&config, &record, Some("m112")).unwrap().read,
        IdentityRead::Current(
            "your name studio in [ai] and [fleet] machine; [identity] git = false, and no \
             pass writes git's identity"
                .to_owned()
        )
    );
    // Without the name, aterm's own write of `studio` takes the roster's label again.
    std::fs::write(
        &config,
        "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"studio\"\n",
    )
    .unwrap();
    write_identity_at(&config, &record, Some("m112")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("m112".to_owned()), Some("m112".to_owned())]
    );
}

/// `sync.identity` keeps the person's machine name, and only theirs: the roster labels an
/// aterm before the record wrote (v0.146 and v0.147, both keys alike, no record) are aterm's,
/// so when the roster's label moves the pass moves them; a pass cut off between recording
/// its write and making it leaves the key at a value the record names, so its own earlier
/// write never reads as the person's either. NEGATIVE CONTROL: a pair that is no roster
/// label, with no record, is the person's.
#[test]
fn an_aterm_written_label_is_not_read_as_the_persons() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    let snapshot = aterm_uds::atomic_file::ConfigFile::open(&config).unwrap();
    // An earlier aterm's write of the label, with no record.
    write_machine_label(&snapshot, "m112", &["ai", "fleet"]).unwrap();
    assert!(!record.exists());
    let read = identity_owed(&config, &record, Some("m113")).unwrap().read;
    assert!(
        matches!(&read, IdentityRead::Due { from, .. }
            if from == "[ai] machine = m112, [fleet] machine = m112"),
        "aterm's own label reads as the person's once the roster's moves: {read:?}"
    );
    // This aterm's write, cut off after its record: the record names both labels.
    record_written(&record, &["ai", "fleet"], "m113", &machines(&config)).unwrap();
    let read = identity_owed(&config, &record, Some("m114")).unwrap().read;
    assert!(matches!(read, IdentityRead::Due { .. }), "{read:?}");
    write_identity_at(&config, &record, Some("m114")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("m114".to_owned()), Some("m114".to_owned())]
    );
    // NEGATIVE CONTROL: the person's own pair, with no record, is kept.
    let theirs = scratch.path().join("theirs.toml");
    let none = scratch.path().join("none/identity-written.toml");
    std::fs::write(
        &theirs,
        "[ai]\nmachine = \"studio\"\n[fleet]\nmachine = \"studio\"\n",
    )
    .unwrap();
    assert!(matches!(
        identity_owed(&theirs, &none, Some("m113")).unwrap().read,
        IdentityRead::Current(_)
    ));
}

/// A name the person sets back in Settings (`[ai] machine`, the field Settings edits) after
/// the roster moved is theirs: the record's earlier values cover only a pass cut off between
/// recording its write and making it, so once that write landed they go, and a value aterm
/// wrote and then replaced is no longer aterm's.
#[test]
fn a_name_the_person_sets_back_stays_theirs() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    write_identity_at(&config, &record, Some("m112")).unwrap();
    // The roster moves this Mac to m113, and the pass follows it: both keys land.
    write_identity_at(&config, &record, Some("m113")).unwrap();
    assert_eq!(
        machines(&config),
        [Some("m113".to_owned()), Some("m113".to_owned())]
    );
    // The person keeps the Mac's old name for their AI sessions.
    std::fs::write(
        &config,
        "[ai]\nmachine = \"m112\"\n[fleet]\nmachine = \"m113\"\n",
    )
    .unwrap();
    let owed = identity_owed(&config, &record, Some("m113")).unwrap();
    assert!(
        owed.write.is_none(),
        "the person's [ai] machine = m112 reads as aterm's: {:?}",
        owed.read
    );
}

/// With no record, a pair of alike roster labels is aterm's (v0.146 and v0.147 wrote them)
/// only where the vault's roster gives that label to this Mac; any other such pair, a label
/// the person typed that looks like a roster label among them, is theirs and kept.
#[test]
fn an_unrecorded_pair_is_aterms_only_where_the_roster_gives_it_to_this_mac() {
    let pair = [Some("m112".to_owned()), Some("m112".to_owned())];
    assert!(unrecorded_label(&pair, &|label| label == "m112"));
    assert!(!unrecorded_label(&pair, &|_| false));
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let record = scratch.path().join("state/converge/identity-written.toml");
    std::fs::write(
        &config,
        "[ai]\nmachine = \"m112\"\n[fleet]\nmachine = \"m112\"\n",
    )
    .unwrap();
    let snapshot = aterm_uds::atomic_file::ConfigFile::open(&config).unwrap();
    let theirs = identity_owed_in(&snapshot, &record, Some("m113"), &|_| false).unwrap();
    assert!(theirs.write.is_none(), "{:?}", theirs.read);
    let ours =
        identity_owed_in(&snapshot, &record, Some("m113"), &|label| label == "m112").unwrap();
    assert!(ours.write.is_some(), "{:?}", ours.read);
}

/// NEGATIVE CONTROL: `read_history`, `record_written`, `confirm_written`, `restore_record`,
/// and `write_identity_at` refuse symlinked `state` or `state/converge` directories without
/// reading, overwriting, or unlinking files in the symlink's target.
#[cfg(unix)]
#[test]
fn identity_record_refuses_symlinked_state_and_converge_directories() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let outside = scratch.path().join("outside");
    std::fs::create_dir_all(outside.join("converge")).unwrap();
    let outside_record = outside.join("converge/identity-written.toml");
    std::fs::write(&outside_record, "ai = \"m112\"\nfleet = \"m112\"\n").unwrap();

    let config = scratch.path().join("aterm.toml");
    let linked_state = scratch.path().join("linked-state");
    std::os::unix::fs::symlink(&outside, &linked_state).unwrap();
    let via_state = linked_state.join("converge/identity-written.toml");

    assert!(identity_owed(&config, &via_state, Some("m113")).is_err());
    assert!(record_written(&via_state, &["ai", "fleet"], "m113", &[None, None]).is_err());
    assert!(confirm_written(&via_state, &["ai"]).is_err());
    assert!(restore_record(&via_state, None).is_err());
    assert!(restore_record(&via_state, Some(b"ai = \"m999\"\n")).is_err());
    assert!(write_identity_at(&config, &via_state, Some("m113")).is_err());
    assert_eq!(
        std::fs::read_to_string(&outside_record).unwrap(),
        "ai = \"m112\"\nfleet = \"m112\"\n"
    );

    let real_state = scratch.path().join("real-state");
    std::fs::create_dir(&real_state).unwrap();
    std::os::unix::fs::symlink(outside.join("converge"), real_state.join("converge")).unwrap();
    let via_converge = real_state.join("converge/identity-written.toml");

    assert!(identity_owed(&config, &via_converge, Some("m113")).is_err());
    assert!(restore_record(&via_converge, None).is_err());
    assert!(write_identity_at(&config, &via_converge, Some("m113")).is_err());
    assert_eq!(
        std::fs::read_to_string(&outside_record).unwrap(),
        "ai = \"m112\"\nfleet = \"m112\"\n"
    );
}
