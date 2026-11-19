// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every word a harness note can carry is a word of the journal's vocabulary and the widest
/// note of each shape the harness makes fits one event, so no note is dropped at the glue;
/// and the two realm tables agree: the harness's claims key and the lab's account key name
/// one account (design §5).
/// NEGATIVE CONTROL: with `event` dropping the other account (`to: None`), the switch names
/// no account: `` called `Option::unwrap()` on a `None` value ``.
#[test]
fn every_harness_word_is_a_journal_word() {
    let account = journal::Key::new("ac-0123456789abcdef").unwrap();
    for kind in journal::KINDS {
        assert!(
            Kind::parse(kind).is_some(),
            "a harness kind the journal does not take: {kind:?}"
        );
    }
    for window in journal::WINDOWS {
        assert!(Window::parse(window).is_some(), "{window}");
    }
    for model in journal::MODELS {
        assert!(ModelBucket::parse(model).is_some(), "{model}");
    }
    for src in journal::SOURCES {
        assert!(Source::parse(src).is_some(), "{src}");
    }
    for res in journal::RESES {
        assert!(Res::parse(res).is_some(), "{res}");
    }
    for why in journal::WHYS {
        assert!(Word::<16>::new(why).is_some(), "{why}");
    }
    for plan in journal::PLANS {
        assert!(Word::<12>::new(plan).is_some(), "{plan}");
    }
    for cli in Cli::ALL {
        assert!(Word::<10>::new(cli.id()).is_some(), "{}", cli.id());
    }
    // The widest note of each shape the harness makes (`observe`): a reading, a wall, a
    // switch, an error with its reason. Each fits one event (192 B).
    let base = Note {
        t: 9_999_999_999,
        kind: "quota",
        account: Some(account),
        cli: "claude",
        sub: None,
        window: Some("7d-model"),
        model: None,
        pct_x10: Some(1000),
        reset: Some(9_999_999_999),
        res: Some("exact"),
        until: None,
        src: "session-usage",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    };
    let shapes = [
        base,
        // A model's own weekly wall names its model's bucket, the longest of them (FL-269).
        Note {
            kind: "wall",
            model: Some("seven_day_overage_included"),
            pct_x10: None,
            reset: None,
            res: None,
            until: Some(9_999_999_999),
            src: "cli-output",
            ..base
        },
        Note {
            kind: "wall-cleared",
            model: Some("seven_day_overage_included"),
            pct_x10: None,
            reset: None,
            res: None,
            src: "cli-output",
            ..base
        },
        Note {
            kind: "switch",
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            src: "cli-output",
            to: journal::Key::new("ac-fedcba9876543210"),
            ..base
        },
        Note {
            kind: "sign-in-failed",
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            src: "signin-flow",
            why: Some("reset-failed"),
            ..base
        },
        // A tab's sighting, with its tab and harness and the believed account of a
        // wrong-account beside it.
        journal::seen_note(
            Cli::Codex,
            account,
            journal::Tab::of("machine", "s-41a2"),
            "codex-alab-agent",
            journal::ObservedSrc::VaultToken,
            journal::SeenWhy::Changed,
            9_999_999_999,
        ),
        journal::wrong_account_note(
            Cli::Claude,
            account,
            journal::Key::new("ac-fedcba9876543210"),
            journal::Tab::of("machine", "s-41a2"),
            journal::ObservedSrc::Profile,
            9_999_999_999,
        ),
        // The machine sweep's sighting of an idle login, which names no tab.
        journal::idle_seen_note(
            Cli::Kimi,
            account,
            journal::ObservedSrc::AuthFile,
            journal::SeenWhy::First,
            9_999_999_999,
        ),
        // The widest plan word an account's plan is noted with.
        journal::plan_note(
            Cli::Claude,
            account,
            "enterprise",
            journal::ObservedSrc::Profile,
            9_999_999_999,
        ),
    ];
    let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
    for note in &shapes {
        let event = event(note).unwrap();
        assert_eq!(event.kind.word(), note.kind);
        assert!(event.encode(&mut buf).is_some(), "{note:?} fits an event");
    }
    let switch = event(&shapes[3]).unwrap();
    assert_eq!(switch.kind, Kind::Switch);
    let wall = event(&shapes[1]).unwrap();
    assert_eq!(
        (wall.kind, wall.model),
        (Kind::Wall, Some(ModelBucket::Fable))
    );
    // A model rides only on a `7d-model` wall, its clearing or its refill (FL-271): the glue
    // drops any other.
    assert_eq!(
        event(&Note {
            model: Some("seven_day_overage_included"),
            ..shapes[0]
        }),
        None,
        "a reading names no model"
    );
    let refill = Note {
        kind: "reset-used",
        until: None,
        model: Some("seven_day_sonnet"),
        ..shapes[1]
    };
    assert_eq!(
        event(&refill).map(|e| (e.kind, e.model)),
        Some((Kind::ResetUsed, Some(ModelBucket::Sonnet)))
    );
    assert_eq!(
        event(&Note {
            window: Some("5h"),
            ..refill
        }),
        None,
        "a refill of the five-hour window names no model"
    );
    assert_eq!(switch.to.unwrap().as_str(), "ac-fedcba9876543210");
    for realm in [
        "claude",
        "anthropic",
        "codex",
        "openai",
        "grok",
        "xai",
        "kimi",
        "kimi:mainland-cn",
        "agy",
        "antigravity",
    ] {
        let lease =
            aterm_agent::harness::claims::LeaseKey::new(realm, "Ann@Example.invalid").unwrap();
        assert_eq!(
            aterm_db::typed::keys::account_pk(&lease.pool, &lease.email).unwrap(),
            aterm_db::typed::keys::account_pk(realm, "ann@example.invalid").unwrap(),
            "{realm}"
        );
    }
    // An agy row's ledger events are keyed as the Google account a read of agy is attributed
    // to (`journal::realm_key("google", …)`): one account, one key, whichever path names it.
    let agy = aterm_agent::harness::claims::LeaseKey::for_cli(
        aterm_agent::harness::accounts::Cli::Agy,
        None,
        "Ann@Example.invalid",
    )
    .unwrap();
    assert_eq!(
        aterm_db::typed::keys::account_pk(&agy.pool, &agy.email).unwrap(),
        aterm_db::typed::keys::account_pk("google", "ann@example.invalid").unwrap(),
    );
    // CONTROL: another realm's key for the same address differs, so the equality above is
    // the mapping's and not a keyer that ignores its realm.
    assert_ne!(
        aterm_db::typed::keys::account_pk(&agy.pool, &agy.email).unwrap(),
        aterm_db::typed::keys::account_pk("meta", "ann@example.invalid").unwrap(),
    );
}

/// EVERY PLAN ROW OF A PROVIDER WHOSE CLI STATES ITS PLAN CAN BE OBSERVED. The accounts relation
/// reads an observed plan as `<provider>-<word>` and compares it with the plan row a person
/// set, a row that adds a tier to the observed word included (`anthropic-max-20x` beside an
/// observed `max`). So where a provider's CLI states its plan (some bought row of the provider
/// is a word `plan_word` keeps), every row of that provider is such a word, or one with a tier
/// after it: a row that is neither (`openai-prolite`, were `prolite` no plan word) could be
/// set but never observed, since its `plan-seen` would never be written. A provider whose CLIs
/// state no plan (Google, xAI) has rows only a person sets.
/// NEGATIVE CONTROL: with `prolite` out of `journal::PLANS`, `` the registry's plan
/// openai-prolite is no plan word a Mac writes ``.
#[test]
fn every_plan_the_registry_seeds_is_a_plan_word_a_mac_writes() {
    let word_of = |seed: &aterm_db::alab::seeds::PlanSeed| {
        seed.pk
            .strip_prefix(seed.provider)
            .and_then(|rest| rest.strip_prefix('-'))
            .unwrap_or_else(|| panic!("{} is keyed <provider>-<word>", seed.pk))
    };
    // The word a Mac writes of a row: the row's own word, or the word its tier follows.
    let written = |word: &str| {
        journal::plan_word(word).or_else(|| {
            let (base, tier) = word.rsplit_once('-')?;
            journal::plan_word(base).filter(|_| !tier.is_empty())
        })
    };
    let states_plans: std::collections::BTreeSet<&str> = aterm_db::alab::seeds::PLANS
        .iter()
        .filter(|seed| word_of(seed) != "free" && written(word_of(seed)).is_some())
        .map(|seed| seed.provider)
        .collect();
    let mut seen = 0;
    for seed in &aterm_db::alab::seeds::PLANS {
        if !states_plans.contains(seed.provider) {
            continue;
        }
        assert!(
            written(word_of(seed)).is_some(),
            "the registry's plan {} is no plan word a Mac writes",
            seed.pk
        );
        seen += 1;
    }
    assert!(seen >= 7, "the registry seeds its plans: {seen}");
}

/// The backfill is marked done only once its notes are in the spool: a spool that cannot
/// be written leaves it unmarked, so the next start backfills again and no note is lost.
/// NEGATIVE CONTROL: with `record` marking first (`journal::mark_backfilled(state)?` before
/// the appends), the failed backfill is marked done: `` the failed backfill is not marked ``.
#[test]
fn a_backfill_is_marked_only_once_its_notes_are_spooled() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("harness");
    std::fs::create_dir_all(&state).unwrap();
    let blocked = scratch.path().join("alab");
    std::fs::write(&blocked, b"").unwrap();
    let spool = Spool::at(blocked.clone());
    let note = Note {
        t: 1_800_000_000,
        kind: "signed-in",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "claude",
        sub: None,
        window: None,
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "cli-output",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    };
    assert!(record(&spool, &state, &[note]).is_err());
    assert!(
        !journal::backfilled(&state),
        "the failed backfill is not marked"
    );
    std::fs::remove_file(&blocked).unwrap();
    record(&spool, &state, &[note]).unwrap();
    assert!(journal::backfilled(&state));
    let spooled =
        std::fs::read_to_string(blocked.join(aterm_db::alab::journal::spool::SPOOL)).unwrap();
    assert_eq!(spooled.lines().count(), 1);
}

/// THE UPGRADE PAST FL-269, end to end: the build before it left its backfill marked done and
/// its note of a standing Fable wall in the spool, a `7d-model` wall that names no model, which
/// no reader takes as any model's wall, and the harness notes no unchanged wall again. The
/// next start's backfill (`backfill_at`) notes that wall once more, now with Fable's bucket,
/// and marks itself done; the flusher's fold then states it in the account's `ai/usage` row
/// under `model_walls`, not in `wall`, and `ai/limits` and rotation read it there.
/// NEGATIVE CONTROL, run: with `journal::backfilled` taking any marker as done (`.is_some()`),
/// the start (`start_backfill`) would not backfill again: `` the older build's marker ``.
#[test]
fn after_an_upgrade_each_standing_model_wall_reaches_the_usage_row_with_its_model() {
    use aterm_agent::harness::accounts::{Account, State, Store, Wall};
    use aterm_db::alab::journal::fold_limits_for_test;
    const ANN: &str = "ac-0123456789abcdef";
    const MAC: &str = "0000000000000000000000000000beef";
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let now: i64 = 1_800_000_000;
    let until = now + 86_400;
    let store = Store::at_for(&state, Cli::Claude, &home);
    let mut ann = Account {
        pk: Some(ANN.into()),
        ..Account::default()
    };
    ann.walls.insert(
        "seven_day_overage_included".into(),
        Wall {
            until: Some(until),
            at: now - 600,
            by: String::new(),
            attempts: 0,
            cleared: false,
            seen: None,
        },
    );
    let mut held = State::default();
    held.accounts.insert("ann".into(), ann);
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    aterm_uds::private_file::ensure_private_dir(store.state_file().parent().unwrap()).unwrap();
    aterm_uds::private_file::write_private(
        store.state_file().parent().unwrap(),
        "state.json",
        (held.to_json() + "\n").as_bytes(),
    )
    .unwrap();
    // What the older build left: its marker, and its note of the wall, with no model.
    let mark = state.join("alab");
    aterm_uds::private_file::ensure_private_dir(&mark).unwrap();
    aterm_uds::private_file::write_private(&mark, "backfilled", b"1\n").unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let older = event(&Note {
        t: u64::try_from(now - 600).unwrap(),
        kind: "wall",
        account: journal::Key::new(ANN),
        cli: "claude",
        sub: None,
        window: Some("7d-model"),
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: Some(u64::try_from(until).unwrap()),
        src: "cli-output",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    })
    .unwrap();
    assert!(matches!(spool.append(&older), Ok(Appended::Written)));
    assert!(!journal::backfilled(&state), "the older build's marker");

    backfill_at(&spool, &state, &home, None, now).unwrap();
    assert!(journal::backfilled(&state));
    let folded = fold_limits_for_test(&[(MAC, &spool)], &[], &[], now.unsigned_abs()).unwrap();
    let usage = &folded.usage[&format!("{ANN}.{MAC}")];
    let fable = aterm_json::json!([
        {"bucket": "seven_day_overage_included", "until": until.unsigned_abs() * 1000}
    ]);
    assert_eq!(
        (usage.get("wall"), usage.get("model_walls")),
        (Some(&aterm_json::json!("none")), Some(&fable)),
        "the standing Fable wall reaches the fleet with its model: {usage:?}"
    );
    assert_eq!(folded.limits[ANN].get("model_walls"), Some(&fable));
    assert_eq!(
        folded.rotation.model_walls.get(ANN),
        Some(
            &[(
                "seven_day_overage_included".to_owned(),
                until.unsigned_abs() * 1000
            )]
            .into_iter()
            .collect()
        )
    );
}

/// THE FLEET SAYS WHAT THE STORE HOLDS, end to end: each step is a real account transaction
/// (the recorders the harness calls, through `Store::transact_logins`, whose commit runs
/// `journal::observe` on the ledger rows it writes), its notes go through the glue's `event`
/// into a spool, and after every one the three folds a Mac's sign-in word is read from agree
/// with the login word the store then holds: this Mac's book (its `use` row and metrics
/// row), a peer that holds its events, and a peer that holds its `use` row too. The steps are
/// the audit's cases: a login wall, a failed sign-in, a renewal whose attempt row follows it
/// in the same second, a failed and a wrong-account sign-in of a login that still works, a
/// served turn that proves a login with no auth row, a displacement, a struck canary, a wall
/// older than the word the store holds, a forgotten row, and two changes in one second.
/// NEGATIVE CONTROLS: with `of_ledger` mapping every `Kind::SignIn` row to `sign-in-failed`
/// and `settle` telling every row's word (as before), the begun attempt is published as a
/// failure and the word list differs (`assertion `left == right` failed`; the state's own
/// word, told after, still heals the renewal); with `settle`'s closing comparison skipped,
/// `` a served turn proves it: the store holds Ok, the folds FoldedSignIn { book:
/// Some("ended"), … } ``.
#[test]
fn the_fleet_fold_ends_on_the_sign_in_state_the_store_holds() {
    use aterm_agent::harness::accounts::{
        self, AttemptOutcome, Auth, Event as Row, Logins, State, Store, Strike,
    };
    use aterm_db::alab::journal::fold_signin_for_test;
    const ANN: &str = "ac-0123456789abcdef";
    const BOB: &str = "ac-fedcba9876543210";
    type Step = fn(&mut State, &mut Logins, i64) -> Vec<Row>;
    fn key(state: &mut State) {
        for (label, pk) in [("ann", ANN), ("bob", BOB)] {
            state.accounts.entry(label.into()).or_default().pk = Some(pk.into());
        }
    }
    // (what happened, seconds since the step before, the transaction's edit)
    let steps: [(&str, i64, Step); 17] = [
        ("first seen signed in", 60, |state, _, at| {
            key(state);
            accounts::note_auth(state, "ann", Auth::Ok, at)
        }),
        ("a login wall", 60, |state, logins, at| {
            accounts::note_login_end(state, logins, "ann", at)
        }),
        ("a sign-in begun", 60, |_, _, at| {
            vec![
                Row::sign_in(at, "ann", accounts::SignInStep::Begun)
                    .because("shared preparation begun"),
            ]
        }),
        ("a sign-in that failed", 60, |_, logins, at| {
            accounts::note_sign_in_attempt(logins, "ann", AttemptOutcome::Failed, "device", at)
        }),
        (
            "renewed, its attempt row after it",
            60,
            |state, logins, at| {
                let mut rows =
                    accounts::note_renewed(state, logins, "ann", None, "/logins/ann-2", None, at);
                rows.extend(accounts::note_sign_in_attempt(
                    logins,
                    "ann",
                    AttemptOutcome::SignedIn,
                    "device",
                    at,
                ));
                rows
            },
        ),
        (
            "a failed sign-in of a working login",
            60,
            |_, logins, at| {
                accounts::note_sign_in_attempt(logins, "ann", AttemptOutcome::Blocked, "page", at)
            },
        ),
        (
            "a wrong account, the login still works",
            60,
            |_, logins, at| accounts::note_wrong_account(logins, "ann", at),
        ),
        ("a browser window ended", 60, |_, _, at| {
            vec![
                Row::sign_in(at, "ann", accounts::SignInStep::Window)
                    .because("x window closed no-session"),
            ]
        }),
        ("a login wall again", 60, |state, logins, at| {
            accounts::note_login_end(state, logins, "ann", at)
        }),
        ("a served turn proves it", 60, |state, _, at| {
            state.active = Some("ann".into());
            state.proven = false;
            accounts::prove(state, "ann", at).into_iter().collect()
        }),
        (
            "displaced from the default login",
            60,
            |state, logins, at| accounts::note_displaced(state, logins, "ann", "bob", at),
        ),
        (
            "served again after being signed out",
            60,
            |state, logins, at| accounts::observe_served(state, logins, "ann", at),
        ),
        ("a canary struck signed out", 60, |state, _, at| {
            accounts::strike(state, "ann", "s-1", &Strike::SignedOut, at)
        }),
        ("signed in, in the same second", 0, |state, _, at| {
            accounts::note_auth(state, "ann", Auth::Ok, at)
        }),
        (
            "a wall older than the word held",
            60,
            |state, logins, at| accounts::note_login_end(state, logins, "ann", at - 3600),
        ),
        ("the row forgotten", 60, |state, logins, _| {
            accounts::forget_account(state, logins, "ann");
            Vec::new()
        }),
        ("the row back, signed out", 60, |state, _, at| {
            key(state);
            accounts::note_auth(state, "ann", Auth::SignedOut, at)
        }),
    ];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at_for(
        &scratch.path().join("harness"),
        Cli::Claude,
        &scratch.path().join("home"),
    );
    let spool = Spool::at(scratch.path().join("alab"));
    let ann = Pk::new(ANN).unwrap();
    let mut now = 1_800_000_000;
    let mut words = Vec::new();
    for (what, later, step) in steps {
        now += later;
        let (done, notes) = journal::capture(|| {
            store.transact_logins(now, &mut None, |state, logins| Ok(step(state, logins, now)))
        });
        done.unwrap_or_else(|why| panic!("{what}: {why}"));
        for note in &notes {
            let event = event(note).unwrap_or_else(|| panic!("{what}: {note:?} is no event"));
            assert!(
                matches!(spool.append(&event), Ok(Appended::Written)),
                "{what}"
            );
        }
        let (state, _) = store.load(now).unwrap();
        let held = state
            .accounts
            .get("ann")
            .map_or(Auth::Unseen, accounts::Account::auth_on_record);
        let folds = fold_signin_for_test(&spool, &ann, now.unsigned_abs()).unwrap();
        for fold in [folds.book, folds.peer_events, folds.peer] {
            let word = fold.map_or(Some(journal::SignWord::Unknown), journal::SignWord::parse);
            assert!(
                word.is_some_and(|word| word.agrees(held)),
                "{what}: the store holds {held:?}, the folds {folds:?}"
            );
        }
        assert_eq!(folds.book, folds.peer, "{what}");
        assert_eq!(folds.book, folds.peer_events, "{what}");
        words.push(folds.book);
    }
    // Which word, not only that it agrees: a success is `in`, a begun attempt changes
    // nothing, a heal is heard, and a forgotten row leaves no word.
    assert_eq!(
        words,
        [
            Some("in"),
            Some("ended"),
            Some("ended"),
            Some("failed"),
            Some("in"),
            Some("in"),
            Some("in"),
            Some("in"),
            Some("ended"),
            Some("in"),
            Some("out"),
            Some("in"),
            Some("out"),
            Some("in"),
            Some("in"),
            None,
            Some("out"),
        ]
    );
}

/// Before any harness has created its state directory, or when the clock is not yet past the
/// Unix epoch, `backfill_at` leaves the backfill unmarked instead of creating `<state>/alab`
/// and sealing an empty pass; and `event` rejects zero-timestamp or over-100% quota notes
/// rather than stripping `pct_x10`.
/// NEGATIVE CONTROL: with `backfill` calling `record` unconditionally on `!state.is_dir()`,
/// `journal::mark_backfilled` creates `state` and marks it done before any harness runs.
#[test]
fn missing_harness_state_or_pre_epoch_clock_leaves_backfill_unmarked_and_bad_notes_drop() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let missing_state = scratch.path().join("no-harness-yet");
    backfill_at(&spool, &missing_state, scratch.path(), None, 1_800_000_000).unwrap();
    assert!(!missing_state.exists());
    assert!(!journal::backfilled(&missing_state));

    let present_state = scratch.path().join("harness");
    std::fs::create_dir_all(&present_state).unwrap();
    backfill_at(&spool, &present_state, scratch.path(), None, 0).unwrap();
    assert!(!journal::backfilled(&present_state));

    backfill_at(&spool, &present_state, scratch.path(), None, 1_800_000_000).unwrap();
    assert!(journal::backfilled(&present_state));

    let account = journal::Key::new("ac-0123456789abcdef").unwrap();
    let zero_time = Note {
        t: 0,
        kind: "signed-in",
        account: Some(account),
        cli: "claude",
        sub: None,
        window: None,
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "cli-output",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    };
    assert!(event(&zero_time).is_none());
    let over_pct = Note {
        t: 1_800_000_000,
        kind: "quota",
        window: Some("5h"),
        pct_x10: Some(1001),
        ..zero_time
    };
    assert!(event(&over_pct).is_none());
}

/// A note's workspace and a realm-only probe reach the journal as they are: the workspace as
/// the event's `s`, an agy read with no login as a `probe` naming no account; a note of any
/// other kind without an account is dropped at the glue, as every reader would drop it.
/// NEGATIVE CONTROL: with `event` mapping no workspace (`sub: None`), the reading's `s` is
/// lost: `` left: None right: Some(0123456789ab) ``.
#[test]
fn a_workspace_and_a_realm_only_probe_reach_the_journal() {
    let reading = Note {
        t: 1_800_000_000,
        kind: "quota",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "codex",
        sub: journal::SubKey::new("0123456789ab"),
        window: Some("7d"),
        model: None,
        pct_x10: Some(250),
        reset: Some(1_800_003_600),
        res: Some("exact"),
        until: None,
        src: "rollout",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    };
    assert_eq!(event(&reading).unwrap().sub, Word::new("0123456789ab"));
    let probe = Note {
        kind: "probe",
        account: None,
        cli: "agy",
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        src: "probe",
        why: Some("launching"),
        ..reading
    };
    let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
    let noted = event(&probe).unwrap();
    let len = noted.encode(&mut buf).unwrap();
    let line = &buf[..len];
    assert_eq!(Event::from_line(line), Some(noted), "every reader takes it");
    assert_eq!(noted.account, None);
    let signin = Note {
        kind: "signed-in",
        src: "signin-flow",
        why: None,
        ..probe
    };
    assert_eq!(event(&signin), None);
}

/// A note made while `[alab] journal = false` is never spooled, so no pass can publish it
/// once the journal is on again; one made after it is turned on is.
/// NEGATIVE CONTROL: with `Glue::note` not asking `publishing` (its check removed), the
/// sign-in noted while off is spooled: `` nothing is spooled while off ``.
#[test]
fn a_note_made_while_the_journal_is_off_is_never_spooled() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    let glue = Glue {
        spool: Some(Spool::at(scratch.path().join("alab"))),
        publishing: Publishing::new(Some(config.clone())),
    };
    let note = Note {
        t: 1_800_000_000,
        kind: "signed-in",
        account: journal::Key::new("ac-0123456789abcdef"),
        cli: "claude",
        sub: None,
        window: None,
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        src: "signin-flow",
        why: None,
        to: None,
        tab: None,
        harness: None,
        plan: None,
    };
    let spooled = scratch
        .path()
        .join("alab")
        .join(aterm_db::alab::journal::spool::SPOOL);
    assert!(!glue.note(&note), "a note made while off is not taken");
    assert!(!spooled.exists(), "nothing is spooled while off");
    std::fs::write(&config, "[alab]\njournal = true\n").unwrap();
    assert!(glue.note(&note), "a spooled note is taken");
    assert!(spooled.exists(), "on again, the note is spooled");
}

/// With `[alab] journal = false` the backfill is marked done and notes nothing, so turning
/// the journal on again publishes nothing from before.
/// NEGATIVE CONTROL: with `backfill_at` not reading `publishing` (its check removed), the
/// ledger's sign-in is spooled: `` nothing is spooled while off ``.
#[test]
fn a_backfill_with_the_journal_off_is_marked_and_notes_nothing() {
    use aterm_agent::harness::accounts::{Event as Row, Kind as RowKind, Store};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let now = 1_800_000_000;
    let store = Store::at_for(&state, Cli::Claude, &home);
    store
        .append(&[Row::new(now - 600, RowKind::SignedIn, "ann")])
        .unwrap();
    let mut keyed = accounts::State::default();
    keyed.accounts.insert(
        "ann".into(),
        accounts::Account {
            pk: Some("ac-0123456789abcdef".into()),
            ..accounts::Account::default()
        },
    );
    aterm_uds::private_file::write_private(
        store.state_file().parent().unwrap(),
        "state.json",
        (keyed.to_json() + "\n").as_bytes(),
    )
    .unwrap();
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    backfill_at(&spool, &state, &home, Some(&config), now).unwrap();
    assert!(journal::backfilled(&state));
    assert!(
        !spool
            .dir()
            .join(aterm_db::alab::journal::spool::SPOOL)
            .exists(),
        "nothing is spooled while off"
    );
    // On, the same backfill notes the sign-in: the check above is what kept it out.
    let on = scratch.path().join("on");
    std::fs::create_dir_all(&on).unwrap();
    std::fs::rename(state.join("alab"), on.join("mark")).unwrap();
    backfill_at(&spool, &state, &home, None, now).unwrap();
    assert!(
        spool
            .dir()
            .join(aterm_db::alab::journal::spool::SPOOL)
            .exists()
    );
}

/// A Mac that lists no account holds the login its default home holds, and a default row
/// whose place holds another account's login reports that account, never its own.
/// NEGATIVE CONTROL: with `holdings_in` passing no home (`observe::sweep::places_of(cli,
/// &roster, None, on_token)`), the Mac with no roster reports nothing: `left: Some([]) right: Some([Holding {
/// .. Fill }])`.
#[test]
fn a_default_home_login_no_row_lists_is_held() {
    use aterm_agent::harness::{
        accounts::{Row, Vendor},
        alab_roster::{LabAccount, MachineRef},
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let account = |email: &str, preferred: Option<&str>| LabAccount {
        pk: aterm_db::typed::keys::account_pk("claude", email).unwrap(),
        cli: Cli::Claude,
        vendor: Vendor::default(),
        label: "claude-aaaaaa".into(),
        legacy_label: None,
        email: email.into(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .map(|machine| {
                (
                    MachineRef::from_registry(format!("fixture-{machine}"), Some(machine.into())),
                    Some(1),
                )
            })
            .into_iter()
            .collect(),
    };
    let (uni, ann) = ("uni@example.test", "ann@example.test");
    let lab = Lab {
        me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
        accounts: vec![account(uni, None), account(ann, Some("m3"))],
        ..Lab::default()
    };
    let held = |why| {
        Some(vec![Holding {
            account: Pk::new(&lab.accounts[0].pk).unwrap(),
            why,
        }])
    };
    std::fs::write(
        home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{uni}"}}}}"#),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        held(Why::Fill)
    );
    let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
    let row = Row {
        label: "m22".into(),
        email: Some(ann.into()),
        ..Row::default()
    };
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        held(Why::Fill),
        "the default row's place holds another account"
    );
    std::fs::remove_file(home.join(".claude.json")).unwrap();
    assert_eq!(
        holdings_in(&lab, &config, Some(&home), &OnToken::none()),
        Some(vec![Holding {
            account: Pk::new(&lab.accounts[1].pk).unwrap(),
            why: Why::Borrow,
        }]),
        "nothing read: the row, the belief"
    );
}

/// The flusher's holdings come from every pooled CLI's roster beside `aterm.toml`, keyed by
/// the lab's account key with the harness's reason; a lab that names no machine for this Mac,
/// or a roster that is there and does not read, reports nothing known (`None`), so no place
/// row ends on a passing failure; a CLI with no roster and no login in its home holds nothing.
/// NEGATIVE CONTROL: with `holdings_in` skipping a roster that does not read (`continue`
/// instead of `?`), the broken roster reports `Some([])` and every place row would end
/// (`left: Some([]) right: None`).
#[test]
fn holdings_come_from_the_rosters_beside_the_config() {
    use aterm_agent::harness::{
        accounts::{Row, Vendor},
        alab_roster::{LabAccount, MachineRef},
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let email = "ann@example.test";
    let key = aterm_db::typed::keys::account_pk("claude", email).unwrap();
    let lab = Lab {
        me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
        accounts: vec![LabAccount {
            pk: key.clone(),
            cli: Cli::Claude,
            vendor: Vendor::default(),
            label: "claude-aaaaaa".into(),
            legacy_label: None,
            email: email.into(),
            identity: None,
            browser: None,
            joinable: true,
            preferred: vec![(
                MachineRef::from_registry("fixture-m3", Some("m3".into())),
                Some(1),
            )],
        }],
        ..Lab::default()
    };
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        Some(Vec::new()),
        "no roster"
    );
    let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
    let row = Row {
        label: "claude-aaaaaa".into(),
        dir: Some("/scratch/ann".into()),
        email: Some(email.into()),
        ..Row::default()
    };
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        Some(vec![Holding {
            account: Pk::new(&key).unwrap(),
            why: Why::Borrow,
        }])
    );
    let unnamed = Lab {
        me: None,
        ..lab.clone()
    };
    assert_eq!(holdings_in(&unnamed, &config, None, &OnToken::none()), None);
    std::fs::write(&roster, "[[account]]\nlabel = 7\n").unwrap();
    assert_eq!(
        holdings_in(&lab, &config, None, &OnToken::none()),
        None,
        "a broken roster"
    );
}

/// A PLAN ATERM READS REACHES THE ACCOUNTS' OBSERVED PLAN, once per change of word. The machine
/// sweep reads Claude Code's profile (`oauthAccount.organizationType`) and Codex's `auth.json`
/// (its tokens' `chatgpt_plan_type`); each account's plan becomes one `plan-seen` through the
/// glue's `event` into a spool, and what this Mac publishes of the account (its book, its `use`
/// row's `plan` and its `ev` batch, which the fleet's accounts relation reads as `plan_observed`,
/// `plan_at`) states the word and when. A later look writes nothing; a changed word is written
/// once; a plan outside the vocabulary (Claude Code's other organization types, a plan claim
/// that is no plan word) and a CLI that states none (Muse Code) write nothing. A canary sits in
/// every other field of the fixture files (a credential, a claim, a token's signature, the plan
/// claim itself once): no note, spool file, sweep file, fold or log line carries it. What a peer
/// folds of this Mac's `ev` batch and `use` row (`Seen::plan`, which the accounts relation
/// reads as `plan_observed`) states the same word and time.
/// NEGATIVE CONTROLS: with `event` dropping the note's plan (`plan: None`, as before), no event
/// holds the plan: `` left: None right: Some(("max", 1800000000)) ``; with `sweep` noting a plan
/// whatever word it last noted, the second look writes again: `` left: [("claude", "max"),
/// ("codex", "plus")] right: [] ``; with `journal::plan_word` taking any word-shaped text, the
/// canary plan claim is written: `` a plan-seen of codex carries the canary ``; with a peer's
/// `Seen` keeping no plan it folds, `` left: None right: Some(("max", 1800000000)) `` at the
/// `peer` check; with `prolite` out of `journal::PLANS`, `` left: [("claude", "pro")] right:
/// [("claude", "pro"), ("codex", "prolite")] ``.
#[cfg(unix)]
#[test]
fn a_plan_aterm_reads_is_written_once_and_read_as_the_accounts_observed_plan() {
    use aterm_agent::harness::observe::sweep;
    use aterm_db::alab::journal::fold_plan_for_test;
    // Word-shaped (12 bytes of `[a-z0-9]`), so only the plan vocabulary keeps it out.
    const CANARY: &str = "zqcanary5b7d";
    const ANN: &str = "ann@example.invalid";
    const T0: i64 = 1_800_000_000;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let state = scratch.path().join("harness");
    let spool = Spool::at(scratch.path().join("alab"));
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let profile = |organization: &str| {
        format!(
            r#"{{"primaryApiKey":"{CANARY}","oauthAccount":{{"emailAddress":"{ANN}","accountUuid":"11111111-2222-3333-4444-555555555555","organizationType":"{organization}","organizationName":"{CANARY}","billingType":"{CANARY}"}}}}"#
        )
    };
    let codex_auth = |plan: &str| {
        let part = |bytes: &[u8]| {
            aterm_codec::base64::encode(bytes)
                .unwrap()
                .trim_end_matches('=')
                .replace('+', "-")
                .replace('/', "_")
        };
        let claims = format!(
            r#"{{"email":"{ANN}","sid":"{CANARY}","https://api.openai.com/auth":{{"chatgpt_account_id":"acct-1","chatgpt_user_id":"user-1","chatgpt_plan_type":"{plan}","chatgpt_subscription_active_until":"{CANARY}"}}}}"#
        );
        let token = format!(
            "{}.{}.{CANARY}",
            part(br#"{"alg":"none"}"#),
            part(claims.as_bytes())
        );
        format!(
            r#"{{"auth_mode":"chatgpt","OPENAI_API_KEY":"{CANARY}","tokens":{{"id_token":"{token}","access_token":"{token}","refresh_token":"{CANARY}","account_id":"acct-1"}}}}"#
        )
    };
    let claude_file = home.join(".claude.json");
    let codex_file = home.join(".codex/auth.json");
    write(&claude_file, &profile("claude_max"));
    write(&codex_file, &codex_auth("plus"));
    write(
        &home.join(".config/muse/auth.json"),
        &format!(
            r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{ANN}","access_token":"{CANARY}"}}}}}}"#
        ),
    );
    let pk = |cli: Cli| {
        let (realm, email) = journal::claims_of(cli, ANN).unwrap();
        Pk::new(&keyer(&realm, &email).unwrap()).unwrap()
    };
    let (claude, codex) = (pk(Cli::Claude), pk(Cli::Codex));
    // One look of the sweep at `now`, its notes spooled: the plan-seen notes, by CLI and word,
    // and every CLI it noted anything of.
    let look = |now: i64| {
        let ((_, notes), logged) =
            crate::test_log::logged_on_this_thread(aterm_log::Level::Trace, || {
                journal::capture(|| {
                    journal::with_keyer(keyer, || {
                        sweep::run(&state, &home, &[], &OnToken::none(), now)
                    })
                })
            });
        for line in &logged {
            assert!(!line.contains(CANARY), "a log line carries the canary");
        }
        for note in &notes {
            assert!(
                !format!("{note:?}").contains(CANARY),
                "a {} of {} carries the canary",
                note.kind,
                note.cli
            );
            if let Some(event) = event(note) {
                assert!(matches!(spool.append(&event), Ok(Appended::Written)));
            }
        }
        let mut plans: Vec<_> = notes
            .iter()
            .filter(|note| note.kind == "plan-seen")
            .map(|note| (note.cli, note.plan.unwrap_or_default()))
            .collect();
        plans.sort_unstable();
        let clis: std::collections::BTreeSet<_> = notes.iter().map(|note| note.cli).collect();
        (plans, clis)
    };
    let observed = |account: &Pk, now: i64| {
        let folded = fold_plan_for_test(&spool, account, now.unsigned_abs()).unwrap();
        assert!(!format!("{folded:?}").contains(CANARY));
        folded
    };
    let at = |word: &str, t: i64| Some((word.to_owned(), t.unsigned_abs()));

    let (plans, clis) = look(T0);
    assert_eq!(plans, [("claude", "max"), ("codex", "plus")]);
    assert!(
        clis.contains("muse"),
        "Muse Code's login was read: {clis:?}"
    );
    for (account, word) in [(&claude, "max"), (&codex, "plus")] {
        let folded = observed(account, T0);
        assert_eq!(folded.book, at(word, T0));
        assert_eq!(folded.use_row, at(word, T0));
        assert_eq!(folded.events, [(word.to_owned(), T0.unsigned_abs())]);
        assert_eq!(folded.peer, at(word, T0));
    }

    // A later look of the same words writes none, in this process or another.
    let (plans, _) = look(T0 + 900);
    assert_eq!(plans, []);

    // The plan changed: one plan-seen of the new word, which the account now reads. Codex's
    // `prolite` is the registry's `openai-prolite`, so it is a word too.
    let t = T0 + 1_800;
    write(&claude_file, &profile("claude_pro"));
    write(&codex_file, &codex_auth("prolite"));
    let (plans, _) = look(t);
    assert_eq!(plans, [("claude", "pro"), ("codex", "prolite")]);
    let folded = observed(&claude, t);
    assert_eq!(folded.book, at("pro", t));
    assert_eq!(folded.use_row, at("pro", t));
    assert_eq!(folded.peer, at("pro", t));
    assert_eq!(observed(&codex, t).peer, at("prolite", t));
    assert_eq!(
        folded.events,
        [
            ("max".to_owned(), T0.unsigned_abs()),
            ("pro".to_owned(), t.unsigned_abs())
        ]
    );

    // A plan outside the vocabulary is none: nothing is written, and the account keeps its word.
    write(&claude_file, &profile("claude_zero"));
    write(&codex_file, &codex_auth(CANARY));
    let (plans, _) = look(T0 + 2_700);
    assert_eq!(plans, []);
    assert_eq!(observed(&codex, T0 + 2_700).book, at("prolite", t));

    // Nothing this Mac keeps carries the canary: the spool, its book and the sweep's file.
    let mut kept = vec![scratch.path().join("alab"), state.clone()];
    let mut files = 0;
    while let Some(path) = kept.pop() {
        if path.is_dir() {
            kept.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else {
            files += 1;
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                "{} carries the canary",
                path.display()
            );
        }
    }
    assert!(files >= 2, "the spool and the sweep's file were read");
    // The fixtures do carry it, so the absence is the readers' doing.
    assert!(
        std::fs::read_to_string(&codex_file)
            .unwrap()
            .contains(CANARY)
    );
}

/// A harness login row reaches the journal only in the journal's own words (`login`): each word
/// it can say is one of the `login` row's, the times pass as they are, a login with a supervised
/// session is `running`, and a row with a word outside the vocabulary, or keyed by a
/// sub-account, is left out.
/// NEGATIVE CONTROL: with `login` taking any `held` word as `linked` (`mine::Held::parse(..)
/// .or(Some(mine::Held::Linked))`), the misspelt row is kept: `` a word outside is left out ``.
#[test]
fn a_harness_login_row_reaches_the_journal_only_in_its_words() {
    let row = journal::LoginRow {
        account: journal::Key::new("ac-0123456789abcdef").unwrap(),
        cli: "claude",
        place: "managed",
        state: "signed-out",
        proof: "status",
        src: "vault-token",
        tabs: 2,
        held: "probation",
        since: 10,
        served_at: Some(11),
        ends_by: Some(12),
        ended_at: None,
        attempt: "blocked",
        attempt_at: Some(13),
        attempt_route: Some("vault"),
        plan: Some("max"),
        token: Some(login_token::TokenFacts {
            kind: login_token::Kind::SetupToken,
            kept: Some(login_token::Kept::SecretStore),
            issued_at: Some(5),
            expires_at: Some(900),
            refreshed_at: None,
            last_used: Some(11),
            state: login_token::State::Expiring,
        }),
    };
    let taken = login(&row).unwrap();
    assert_eq!(
        taken.cred,
        Some(mine::Credential {
            kind: mine::CredKind::SetupToken,
            kept: Some(mine::Kept::SecretStore),
            issued_at: Some(5),
            expires_at: Some(900),
            refreshed_at: None,
            last_used: Some(11),
            state: mine::CredState::Expiring,
        }),
        "its credential's facts pass as they are"
    );
    assert_eq!(taken.account.as_str(), "ac-0123456789abcdef");
    assert_eq!(
        (taken.state, taken.src, taken.use_, taken.tabs, taken.held),
        (
            mine::State::SignedOut,
            mine::Src::VaultToken,
            mine::Use::Running,
            2,
            mine::Held::Probation
        )
    );
    assert_eq!(
        (
            taken.since,
            taken.served_at,
            taken.ends_by,
            taken.attempt_at
        ),
        (10, Some(11), Some(12), Some(13))
    );
    assert_eq!(
        (
            taken.attempt,
            taken.attempt_route,
            taken.plan.map(|p| p.as_str().to_owned())
        ),
        (
            mine::Attempt::Blocked,
            Some(mine::Route::Vault),
            Some("max".to_owned())
        )
    );
    let idle = login(&journal::LoginRow {
        tabs: 0,
        ..row.clone()
    })
    .unwrap();
    assert_eq!(idle.use_, mine::Use::Idle);
    for word in ["signed-in", "signed-out", "ended", "wrong-account"] {
        assert!(mine::State::parse(word).is_some(), "{word}");
    }
    for word in ["linked", "fill", "borrow", "stranded", "probation"] {
        assert!(mine::Held::parse(word).is_some(), "{word}");
    }
    for word in ["profile", "auth-file", "cli-log", "vault-token"] {
        assert!(mine::Src::parse(word).is_some(), "{word}");
    }
    for word in ["none", "signed-in", "failed", "blocked", "no-route"] {
        assert!(mine::Attempt::parse(word).is_some(), "{word}");
    }
    for cli in Cli::ALL {
        assert!(mine::Cli::parse(cli.id()).is_some(), "{}", cli.id());
    }
    for plan in journal::PLANS {
        assert!(Word::<16>::new(plan).is_some(), "{plan}");
    }
    assert!(
        login(&journal::LoginRow {
            held: "lnked",
            ..row.clone()
        })
        .is_none(),
        "a word outside is left out"
    );
    assert!(
        login(&journal::LoginRow {
            account: journal::Key::new("sa-0123456789abcdef").unwrap(),
            ..row
        })
        .is_none(),
        "a sub-account holds no login"
    );
}

/// THE LOGIN SOURCE READS EACH CLI APART (`logins_in`): a CLI with no roster holds nothing; a
/// Claude Code row's login is what its store says, held `linked` where a preference of this Mac
/// names its account and `fill` otherwise, keyed by the lab's account key; a row the upkeep last
/// read on its vault token is signed in on it, and the reading's next turn (its token's end, the
/// nearer) is reported beside the rows; a CLI whose store does not read, or that keeps its store
/// under a home there is none of, reports `None`, and the other CLIs' reports stand; a lab that
/// names no machine for this Mac reports nothing.
///
/// Each row states the credential its sign-in runs on (SF-34) as the real readers read made-up
/// login files through this source, judged on the records with this Mac's runtime readings in
/// them: ann's Claude Code `.credentials.json` with an access and a refresh token, an `oauth`
/// login in its `login-file` with no end, `valid`; bob's signed-out stored login `expired`, and on
/// his vault token (where a launch here starts on one) a `setup-token` in the `secret-store` with
/// the mint and last use of this Mac's table row of it and its end, `expiring`; cal's Codex
/// `auth.json` with JWT-shaped tokens, a refresh token and its last refresh, an `oauth` login in
/// its `login-file` refreshed then, `unknown` while his word on record was read of no credentials
/// it holds, and `expired` once his login is recorded lapsed under them. The files each row was
/// read from are stamped beside the report, and no 8-byte window of any made-up value is in a
/// print of the reports.
///
/// NEGATIVE CONTROLS, run on macOS in SF-34's last mutation pass: with `logins_in`'s credential
/// reader answering `None`, `` ann states her stored login, bob his signed-out one `` fails; with
/// `logins_in` stamping no row's files (no `watched.push`), `` each row's credential files are
/// stamped `` fails; with `login_token::Judged::of` not hydrating the records, `` his login
/// recorded lapsed under the credentials his auth.json holds `` fails (`unknown`). Run in the
/// package's earlier pass: with `logins_in` leaving the token reading out (no
/// `records.state.tokens_here = store.read_tokens_here()`), bob stays on his profile: `` bob
/// signed in on his vault token `` (`left: (SignedOut, Profile) right: (SignedIn, VaultToken)`);
/// with it keeping no turn (`turns` never set), `` the reading turns when bob's token ends ``
/// (`left: None`); with a store that does not read reported as holding nothing
/// (`out.insert(word, Some(read().unwrap_or_default()))`), `` a store that does not read reports
/// none of its CLI `` (`left: Some([]) right: None`).
#[cfg(unix)]
#[test]
fn logins_come_from_each_clis_store_apart_and_one_that_does_not_read_reports_none_of_its_cli() {
    use aterm_agent::harness::{
        accounts::{Account, Auth, Row, State, Vendor},
        accounts_tokens::{self, InVault, TokenRow, UsedOn, Verification},
        alab_roster::{LabAccount, MachineRef},
        token_route::LAUNCHES_ON_TOKEN,
    };
    use std::collections::BTreeMap;
    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    let b64url = |bytes: &[u8]| {
        let mut out = String::new();
        aterm_codec::base64::encode_url_into(&mut out, bytes).unwrap();
        out
    };
    let jwt = |email: &str, sig: &str| {
        format!(
            "{}.{}.{sig}",
            b64url(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64url(format!(r#"{{"exp":{},"email":"{email}"}}"#, NOW + 8 * DAY).as_bytes())
        )
    };
    // Made-up values only: no real credential is read or named.
    let claude_a = concat!("sk-ant-oat01-", "Bq8nWx3zKm6rTv1hLc9jPf4dGs7aYe2uNo5iRx0Mt");
    let claude_r = concat!("sk-ant-ort01-", "Vp5wZq2xHm9tKb4nRc1hLf6dGs3aYe8uPo7iWx0Qj");
    let codex_a = jwt("cal@x.example", "Nx4KqZ7mWv2tRb9nPc6hLd1gJs8aYe3uQo0iMx5Vk");
    let codex_id = jwt("cal@x.example", "Gq8vNw3yXr6zMc0tGb4kHf7dJs1aPe9uWo2iRx5Bz");
    let codex_r = concat!("rt_", "Kp9nWq3xZm7vKt1bRc4jLf8dGs2aYe6uPo0iNx5Tz");
    let values = [
        claude_a,
        claude_r,
        codex_a.as_str(),
        codex_id.as_str(),
        codex_r,
    ];
    let windows = |text: &str| {
        values
            .iter()
            .flat_map(|value| (0..=value.len() - 8).map(move |at| &value[at..at + 8]))
            .filter(|window| text.contains(window))
            .collect::<Vec<_>>()
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&harness).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let me = MachineRef::from_registry("fixture-m22", Some("m22".into()));
    let (ann, bob) = ("ann@x.example", "bob@x.example");
    let pk = |email: &str| aterm_db::typed::keys::account_pk("claude", email).unwrap();
    let account = |email: &str, label: &str, preferred: bool| LabAccount {
        pk: pk(email),
        cli: Cli::Claude,
        vendor: Vendor::default(),
        label: label.into(),
        legacy_label: None,
        email: email.into(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: if preferred {
            vec![(me.clone(), Some(1))]
        } else {
            Vec::new()
        },
    };
    let lab = Lab {
        me: Some(me.clone()),
        accounts: vec![
            account(ann, "claude-aaaaaa", true),
            account(bob, "claude-bbbbbb", false),
        ],
        ..Lab::default()
    };
    let read = |home: Option<&Path>| logins_in(&lab, &config, &harness, home, NOW).unwrap();
    let claude = |read: &LoginsRead| {
        read.reports[&mine::Cli::Claude]
            .as_ref()
            .expect("Claude Code's store reads")
            .iter()
            .map(|login| {
                (
                    login.account.as_str().to_owned(),
                    login.state,
                    login.held,
                    login.src,
                    login.since,
                )
            })
            .collect::<Vec<_>>()
    };

    let empty = read(Some(&home));
    assert_eq!(empty.reports.len(), Cli::ALL.len(), "a report per CLI");
    assert!(
        empty
            .reports
            .values()
            .all(|report| report.as_deref() == Some(&[][..])),
        "no roster holds nothing: {:?}",
        empty.reports
    );
    assert_eq!(empty.turns, None);

    let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
    let rows = [
        Row {
            label: "claude-aaaaaa".into(),
            dir: Some(scratch.path().join("ann").display().to_string()),
            email: Some(ann.into()),
            ..Row::default()
        },
        Row {
            label: "claude-bbbbbb".into(),
            dir: Some(scratch.path().join("bob").display().to_string()),
            email: Some(bob.into()),
            ..Row::default()
        },
    ];
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &rows).unwrap(),
    )
    .unwrap();
    let store = accounts::Store::at(&harness);
    let mut state = State::default();
    for (label, auth, at) in [
        ("claude-aaaaaa", Auth::Ok, NOW - 100),
        ("claude-bbbbbb", Auth::SignedOut, NOW - 50),
    ] {
        state.accounts.insert(
            label.into(),
            Account {
                auth,
                auth_at: Some(at),
                ..Account::default()
            },
        );
    }
    store.save(&state).unwrap();
    let ann_dir = scratch.path().join("ann");
    aterm_types::fs_restricted::ensure_private_dir(&ann_dir).unwrap();
    std::fs::write(
        ann_dir.join(".credentials.json"),
        aterm_json::json!({"claudeAiOauth": {"accessToken": claude_a, "refreshToken": claude_r,
            "expiresAt": (NOW + 3600) * 1000}})
        .to_string(),
    )
    .unwrap();
    let stated = read(Some(&home));
    assert_eq!(
        claude(&stated),
        [
            (
                pk(ann),
                mine::State::SignedIn,
                mine::Held::Linked,
                mine::Src::Profile,
                (NOW - 100) as u64
            ),
            (
                pk(bob),
                mine::State::SignedOut,
                mine::Held::Fill,
                mine::Src::Profile,
                (NOW - 50) as u64
            ),
        ]
    );
    assert_eq!(stated.turns, None, "no token reading");
    let creds = |read: &LoginsRead, cli: mine::Cli| {
        read.reports[&cli]
            .as_ref()
            .unwrap()
            .iter()
            .map(|login| login.cred)
            .collect::<Vec<_>>()
    };
    let stored = mine::Credential {
        kind: mine::CredKind::OAuth,
        kept: Some(mine::Kept::LoginFile),
        issued_at: None,
        expires_at: None,
        refreshed_at: None,
        last_used: None,
        state: mine::CredState::Valid,
    };
    let keychain = if aterm_agent::ai::connector::claude_default_login_in_file() {
        None
    } else {
        Some(mine::Kept::Keychain)
    };
    assert_eq!(
        creds(&stated, mine::Cli::Claude),
        [
            Some(stored),
            Some(mine::Credential {
                kept: keychain,
                state: mine::CredState::Expired,
                ..stored
            }),
        ],
        "ann states her stored login, bob his signed-out one"
    );
    assert_eq!(
        stated
            .watched
            .iter()
            .map(|(file, _)| file.clone())
            .collect::<Vec<_>>(),
        [
            ann_dir.join(".credentials.json"),
            scratch.path().join("bob").join(".credentials.json"),
        ],
        "each row's credential files are stamped"
    );
    assert!(stated.watched[0].1.is_some() && stated.watched[1].1.is_none());

    let ends = NOW + 600;
    accounts_tokens::update_table(&store, |table| {
        table.rows.insert(
            pk(bob),
            TokenRow {
                key: pk(bob),
                account_id: None,
                label: Some("claude-bbbbbb".into()),
                entry: format!("aterm-claude-token-{}", pk(bob)),
                minted_at: Some(u64::try_from(NOW - 1000).unwrap()),
                expires_at: Some(u64::try_from(ends).unwrap()),
                expiry_from: Some("printed".into()),
                minted_on: Some("m7".into()),
                signer: None,
                used_on: UsedOn {
                    first_served_at: Some(u64::try_from(NOW - 900).unwrap()),
                    last_served_at: Some(u64::try_from(NOW - 50).unwrap()),
                },
                vault: InVault::InVault,
                verified: None,
                verification: Verification::default(),
                removed_at: None,
            },
        );
    })
    .unwrap();
    store
        .note_tokens_here(
            Some(&BTreeMap::from([(
                "claude-bbbbbb".to_owned(),
                u64::try_from(ends).unwrap(),
            )])),
            NOW - 10,
        )
        .unwrap();
    let tokened = read(Some(&home));
    let bob_row = claude(&tokened)[1].clone();
    if LAUNCHES_ON_TOKEN {
        assert_eq!(
            (bob_row.1, bob_row.3),
            (mine::State::SignedIn, mine::Src::VaultToken),
            "bob signed in on his vault token"
        );
        assert_eq!(
            tokened.turns,
            Some(ends),
            "the reading turns when bob's token ends"
        );
        assert_eq!(
            creds(&tokened, mine::Cli::Claude)[1],
            Some(mine::Credential {
                kind: mine::CredKind::SetupToken,
                kept: Some(mine::Kept::SecretStore),
                issued_at: Some(u64::try_from(NOW - 1000).unwrap()),
                expires_at: Some(u64::try_from(ends).unwrap()),
                refreshed_at: None,
                last_used: Some(u64::try_from(NOW - 50).unwrap()),
                state: mine::CredState::Expiring,
            }),
            "bob states his vault token"
        );
    } else {
        assert_eq!(
            (bob_row.1, bob_row.3),
            (mine::State::SignedOut, mine::Src::Profile)
        );
        assert_eq!(tokened.turns, None);
    }

    let codex_roster = accounts::roster_beside_for(&config, Cli::Codex).unwrap();
    let codex_home = home.join(".codex");
    aterm_types::fs_restricted::ensure_private_dir(&codex_home).unwrap();
    std::fs::write(
        codex_home.join("auth.json"),
        aterm_json::json!({"auth_mode": "chatgpt",
            "last_refresh": aterm_types::rfc3339::format_rfc3339(
                u64::try_from(NOW - 2 * DAY).unwrap()),
            "tokens": {"access_token": codex_a, "id_token": codex_id, "refresh_token": codex_r}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &codex_roster,
        accounts::append_rows_for(
            Cli::Codex,
            "",
            &[Row {
                label: "codex-cccccc".into(),
                email: Some("cal@x.example".into()),
                ..Row::default()
            }],
        )
        .unwrap(),
    )
    .unwrap();
    let codex = accounts::Store::at_for(&harness, Cli::Codex, &home);
    let mut codex_state = State::default();
    codex_state.accounts.insert(
        "codex-cccccc".into(),
        Account {
            auth: Auth::Ok,
            auth_at: Some(NOW - 30),
            ..Account::default()
        },
    );
    codex.save(&codex_state).unwrap();
    let codexed = read(Some(&home));
    let cal = mine::Credential {
        refreshed_at: Some(u64::try_from(NOW - 2 * DAY).unwrap()),
        state: mine::CredState::Unknown,
        ..stored
    };
    assert_eq!(
        creds(&codexed, mine::Cli::Codex),
        [Some(cal)],
        "cal states his Codex login, which his CLI renews; his word on record was read of no \
         credentials his auth.json holds, so it says nothing of them"
    );
    codex
        .record_lapse_for_lab(
            &Row {
                label: "codex-cccccc".into(),
                email: Some("cal@x.example".into()),
                ..Row::default()
            },
            NOW - 10,
        )
        .unwrap();
    let lapsed = read(Some(&home));
    assert_eq!(
        creds(&lapsed, mine::Cli::Codex),
        [Some(mine::Credential {
            state: mine::CredState::Expired,
            ..cal
        })],
        "his login recorded lapsed under the credentials his auth.json holds"
    );
    assert!(
        codexed
            .watched
            .iter()
            .any(|(file, stamp)| *file == codex_home.join("auth.json") && stamp.is_some()),
        "cal's auth.json is stamped"
    );
    let said = format!(
        "{:?} {:?} {:?} {:?}",
        stated.reports, tokened.reports, codexed.reports, lapsed.reports
    );
    assert_eq!(windows(&said), Vec::<&str>::new(), "{said}");
    let codex_state = codex.state_file();
    std::fs::create_dir_all(codex_state.parent().unwrap()).unwrap();
    std::fs::write(&codex_state, b"{ not a state").unwrap();
    for home in [Some(home.as_path()), None] {
        let broken = read(home);
        assert_eq!(
            broken.reports[&mine::Cli::Codex],
            None,
            "a store that does not read reports none of its CLI"
        );
        assert_eq!(
            claude(&broken),
            claude(&tokened),
            "Claude Code's report stands"
        );
    }

    let unnamed = Lab {
        me: None,
        ..lab.clone()
    };
    assert_eq!(
        logins_in(&unnamed, &config, &harness, Some(&home), NOW),
        None
    );
}

/// A LOGIN MAY READ OTHERWISE once a file it is read from moves or its clock turns
/// (`moved_since`, `login_files`): before any report it may; a store's state saved again moves a
/// stamp, and so does Claude Code's token table, its checks and the refusals beside the vault
/// state, each among the stores' files; a row's credential file the read stamped
/// (`LoginsRead::watched`) moving moves it too; a turn ahead of the clock waits and one the clock
/// reached does not, as a credential's stated end passing does with no file moving.
/// NEGATIVE CONTROLS, each run: with `moved_since` ignoring the turn (`seen.stamps != stamps`
/// alone), `` the turn came ``; with `login_files` leaving out the store's state files (no
/// `files.extend(store.state_files())`), `` assertion failed:
/// files.contains(&store.state_file()) ``; with it leaving out the token facts' files (no
/// `SetupTokens::files`), `` the token table and its checks ``; with `moved_since` not looking at
/// the watched files, `` a row's credential file moved ``.
#[cfg(unix)]
#[test]
fn a_login_may_read_otherwise_once_its_files_move_or_its_token_reading_turns() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&harness).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let files = login_files(&config, &harness, None);
    let store = accounts::Store::at(&harness);
    assert!(files.contains(&store.state_file()));
    for name in ["tokens.json", "tokens-check.json"] {
        assert!(
            files.contains(&store.state_file().with_file_name(name)),
            "the token table and its checks: {name}"
        );
    }
    assert!(
        login_files(&config, &harness, Some(&home)).contains(
            &aterm_secrets::claude_token::refusals_file(
                &aterm_secrets::client::Paths::locate_for_home(&home)
            )
        ),
        "the refusals beside the vault state"
    );
    assert!(
        files.windows(2).all(|pair| pair[0] < pair[1]),
        "each file once"
    );
    let before = stamps(&files);
    assert!(moved_since(None, &before, 10), "nothing seen yet");
    let credentials = scratch.path().join("ann").join(".credentials.json");
    std::fs::create_dir_all(credentials.parent().unwrap()).unwrap();
    std::fs::write(&credentials, "{}").unwrap();
    let seen = Seen {
        stamps: before.clone(),
        watched: vec![(credentials.clone(), FileStamp::of(&credentials))],
        turns: Some(20),
    };
    assert!(!moved_since(Some(&seen), &before, 19), "nothing moved");
    assert!(moved_since(Some(&seen), &before, 20), "the turn came");
    std::fs::remove_file(&credentials).unwrap();
    assert!(
        moved_since(Some(&seen), &before, 19),
        "a row's credential file moved"
    );
    let seen = Seen {
        watched: Vec::new(),
        ..seen
    };
    store
        .save(&aterm_agent::harness::accounts::State::default())
        .unwrap();
    assert!(
        moved_since(Some(&seen), &stamps(&files), 19),
        "a saved state moves a stamp"
    );
}

/// A CREDENTIAL'S END TURNS THE LOGIN SOURCE BY THE CLOCK ALONE (`logins_in`'s `turns`): a
/// Claude Code login held with no refresh token states its access token's end, `expiring` 10
/// minutes before it, so the source's next turn is that end, with no file moving, and a read at
/// it says `expired`. (A Codex token's end and a refusal still to settle turn the same way:
/// `login_token`'s tests.)
///
/// NEGATIVE CONTROL, run on macOS in SF-34's last mutation pass: with `logins_in` not folding its
/// credential reader's turns (no `turn(read.turns)`), `` the source turns at the end `` fails.
#[cfg(unix)]
#[test]
fn a_credentials_end_turns_the_login_source_by_the_clock() {
    use aterm_agent::harness::accounts::{Account, Auth, Row, State};
    use aterm_agent::harness::alab_roster::MachineRef;
    const NOW: i64 = 1_800_000_000;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let harness = scratch.path().join("harness");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&harness).unwrap();
    let lab = Lab {
        me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
        ..Lab::default()
    };
    let end = NOW + 600;
    let cal_dir = scratch.path().join("cal");
    aterm_types::fs_restricted::ensure_private_dir(&cal_dir).unwrap();
    std::fs::write(
        cal_dir.join(".credentials.json"),
        aterm_json::json!({"claudeAiOauth": {
            "accessToken": concat!("sk-ant-oat01-", "Hz3KqZ8mWv1tRb6nPc4hLd9gJs2aYe7uQo5iMx0Vw"),
            "expiresAt": end * 1000}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        accounts::roster_beside_for(&config, Cli::Claude).unwrap(),
        accounts::append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                label: "claude-cccccc".into(),
                dir: Some(cal_dir.display().to_string()),
                email: Some("cal@x.example".into()),
                ..Row::default()
            }],
        )
        .unwrap(),
    )
    .unwrap();
    let mut state = State::default();
    state.accounts.insert(
        "claude-cccccc".into(),
        Account {
            auth: Auth::Ok,
            auth_at: Some(NOW - 30),
            ..Account::default()
        },
    );
    accounts::Store::at(&harness).save(&state).unwrap();
    let states = |read: &LoginsRead| {
        read.reports[&mine::Cli::Claude]
            .as_ref()
            .unwrap()
            .iter()
            .map(|login| login.cred.map(|cred| (cred.expires_at, cred.state)))
            .collect::<Vec<_>>()
    };
    let read = logins_in(&lab, &config, &harness, Some(&home), NOW).unwrap();
    assert_eq!(read.turns, Some(end), "the source turns at the end");
    assert_eq!(
        states(&read),
        [Some((
            Some(u64::try_from(end).unwrap()),
            mine::CredState::Expiring
        ))]
    );
    let after = logins_in(&lab, &config, &harness, Some(&home), end).unwrap();
    assert_eq!(
        states(&after),
        [Some((
            Some(u64::try_from(end).unwrap()),
            mine::CredState::Expired
        ))],
        "past its end, expired"
    );
    assert_eq!(after.turns, None, "nothing turns past it");
}
