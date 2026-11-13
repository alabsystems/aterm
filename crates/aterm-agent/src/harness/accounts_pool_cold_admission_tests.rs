// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::usage_policy::{Reads, plain_usage, spent_usage};
use super::*;
use crate::harness::restart_stamp::Wanted;

fn decide(case: i64) -> [i64; 3] {
    let world = World::new("cold-admission");
    let work = world.two(Cli::Codex);
    usage_policy::write_context_login(&work, 1);
    let path = world.cfg.join(Cli::Codex.accounts().roster_file);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    // Cases 15 and 16: its restart asked for `main`, the row beside the one it ran on.
    let asks = case == 15 || case == 16;
    roster.rows.retain(|row| asks || row.label == "work");
    let ran = roster
        .rows
        .iter_mut()
        .find(|row| row.label == "work")
        .unwrap();
    ran.email = Some(if case == 3 {
        "another@example.test".into()
    } else {
        "fixture@example.test".into()
    });
    if case == 2 {
        ran.held_by = Some("another-machine".into());
    }
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    if case == 7 || case == 8 || case == 14 {
        aterm_uds::atomic_file::write(
            &(world.cfg.join("aterm.toml")),
            ("[harness]\naccounts='off'\n").as_ref(),
            0o600,
        )
        .unwrap();
    }
    let seat = world.seat();
    let store = seat.store(Cli::Codex);
    let renewed = world.root.join("renewed");
    if case == 10 {
        usage_policy::write_context_login(&renewed, 2);
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    work.to_str(),
                    renewed.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
    }
    if case == 11 {
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.insert(
                    "work".into(),
                    accounts::Binding {
                        dir: Some(work.to_str().unwrap().into()),
                        ..accounts::Binding::default()
                    },
                );
                Ok(Vec::new())
            })
            .unwrap();
        std::fs::write(&path, "").unwrap();
    }
    // Case 15: the asked row holds a reading taken now under its current login, so it is proven
    // with no read and nothing is recorded by taking it.
    let asked_context = (case == 15).then(|| {
        let logins = store.load_logins(NOW).unwrap().0;
        let bound = accounts::bound(&roster, &logins);
        super::super::sample_context(&seat, Cli::Codex, bound.row("main").unwrap(), &logins)
    });
    store
        .transact(NOW, &mut None, |state| {
            if case == 1 || case == 7 {
                state.prepared_accounts.insert("work".into());
            }
            if let Some(context) = &asked_context {
                let fresh = Sample {
                    session_pct: Some(10),
                    at: NOW,
                    ..Sample::default()
                };
                accounts::note_sample(state, "main", fresh, NOW);
                state.accounts.get_mut("main").unwrap().sample_context = Some(context.clone());
            }
            if case == 9 {
                accounts::note_wall(state, "work", "weekly", Some(NOW + HOUR), NOW, NOW);
            }
            Ok(Vec::new())
        })
        .unwrap();
    if case == 4 {
        std::fs::remove_file(work.join("auth.json")).unwrap();
    }
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let after_read = std::sync::Mutex::new(files.each_ref().map(|file| std::fs::read(file).ok()));
    let read = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
        if case == 5 {
            let mut changed = roster.clone();
            changed.rows[0].browser = Some("changed".into());
            std::fs::write(
                &path,
                accounts::append_rows_for(Cli::Codex, "", &changed.rows).unwrap(),
            )
            .unwrap();
        } else if case == 6 {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "work",
                        work.to_str(),
                        work.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        *after_read.lock().unwrap() = files.each_ref().map(|file| std::fs::read(file).ok());
        None
    };
    let auth = if case == 12 {
        let own = world.root.join("unmanaged");
        usage_policy::write_context_login(&own, 1);
        own.join("auth.json")
    } else {
        work.join("auth.json")
    }
    .display()
    .to_string();
    let old_program = if case == 13 || case == 14 {
        Some(crate::harness::codex_alab::lay_package(
            &world.root.join("old-program"),
            false,
        ))
    } else {
        None
    };
    let wanted = asks.then(|| Wanted::Row("main".into()));
    let result = super::super::cold_launch_env_asked(
        &seat,
        Cli::Codex,
        &[("CODEX_AUTH_PATH", &auth)],
        &read,
        Some(old_program.as_ref().unwrap_or(&world.exe)),
        wanted.as_ref(),
    );
    let admitted = result.is_ok();
    let (picked, kept) = result.map_or((None, None), |asked| (asked.pick, asked.kept));
    let rebound = picked.is_some();
    if let Some(picked) = picked {
        if case == 15 {
            assert_eq!((picked.label.as_str(), picked.value), ("main", None));
        } else {
            assert_eq!(case, 10);
            assert_eq!(picked.value.as_deref(), renewed.join("auth.json").to_str());
        }
    }
    assert_eq!(
        kept.as_deref(),
        (case == 16).then_some(super::super::MOVE_UNREAD),
        "case {case}"
    );
    [
        i64::from(admitted),
        i64::from(
            *after_read.lock().unwrap() == files.each_ref().map(|file| std::fs::read(file).ok()),
        ),
        i64::from(rebound),
    ]
}

pub(crate) fn admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        let [admitted, preserved, rebound] = decide(state["case"]);
        next.insert("admitted", admitted);
        next.insert("preserved", preserved);
        next.insert("rebound", rebound);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn cold_launch_admission_conforms_and_catches_unchecked_saved_logins() {
    let model = aterm_models_harness::cold_launch_admission_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = admission_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad.insert("case", 1);
    bad = admission_transition(&bad, "Decide");
    bad.insert("admitted", 1);
    assert!(!model.check_invariant("CurrentAdmission", &bad));
    bad = model.init_state();
    bad.insert("case", 10);
    bad = admission_transition(&bad, "Decide");
    bad.insert("rebound", 0);
    assert!(!model.check_invariant("CurrentBinding", &bad));
    bad = model.init_state();
    bad.insert("case", 15);
    bad = admission_transition(&bad, "Decide");
    bad.insert("rebound", 0);
    assert!(!model.check_invariant("AskedAccountTaken", &bad));
    bad = model.init_state();
    bad.insert("case", 16);
    bad = admission_transition(&bad, "Decide");
    bad.insert("rebound", 1);
    assert!(!model.check_invariant("AskedAccountProven", &bad));
}

fn witness_world(tag: &str) -> (World, PathBuf) {
    let world = World::new(tag);
    let work = world.two(Cli::Claude);
    std::fs::create_dir_all(world.home.join(".claude")).unwrap();
    std::fs::write(work.join(".claude.json"), b"{}").unwrap();
    aterm_uds::atomic_file::write(
        &world.cfg.join("aterm.toml"),
        b"[harness]\naccounts='wall'\n",
        0o600,
    )
    .unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .transact(NOW, &mut None, |state| {
            state.home_accounts.insert("work".into());
            Ok(Vec::new())
        })
        .unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .load(NOW)
        .unwrap()
        .0
        .execution_verdict("work")
        .unwrap();
    (world, work)
}

fn admit_witness(world: &World, login: Option<&Path>) -> ColdLaunchAdmission {
    super::super::cold_launch_admitted(
        &world.seat(),
        Cli::Claude,
        accounts::ProcessLogin {
            login: login.map(|path| path.to_str().unwrap().to_owned()),
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        },
        &NoUsage,
        Some(&world.exe),
    )
    .unwrap()
}

fn validate_intent(world: &World, intent: &ExecutionIntent) -> Result<(), String> {
    super::super::admit_execution(&world.seat(), Cli::Claude, intent).map(drop)
}

fn witness_images(world: &World) -> [Option<Vec<u8>>; 5] {
    let store = world.seat().store(Cli::Claude);
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        world.cfg.join(Cli::Claude.accounts().roster_file),
        store.dir.join("sync-commit.json"),
    ]
    .map(|path| std::fs::read(path).ok())
}

#[test]
fn cold_witness_keeps_the_exact_managed_binding_and_validates_without_writes() {
    let (world, work) = witness_world("cold-witness-managed");
    let admitted = admit_witness(&world, Some(&work));
    assert!(matches!(
        &admitted.intent,
        ExecutionIntent::Stored { owner: Some(_), .. }
    ));
    assert!(admitted.pick.is_none());
    let before = witness_images(&world);
    let grant =
        super::super::admit_execution(&world.seat(), Cli::Claude, &admitted.intent).unwrap();
    assert!(
        world
            .seat()
            .store(Cli::Claude)
            .acquire_within(&[])
            .unwrap()
            .is_none()
    );
    drop(grant);
    validate_intent(&world, &admitted.intent).unwrap();
    assert_eq!(witness_images(&world), before);
    // The same path now has a new binding generation, even with identical command bytes.
    world
        .seat()
        .store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work",
                work.to_str(),
                work.to_str().unwrap(),
                None,
                NOW,
            ))
        })
        .unwrap();
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}

#[test]
fn cold_witness_rejects_same_directory_row_identity_and_local_profile_replacement() {
    for profile_change in [false, true] {
        let (world, work) = witness_world("cold-witness-identity");
        std::fs::write(
            work.join(".claude.json"),
            b"{\"oauthAccount\":{\"accountUuid\":\"first-principal\"}}",
        )
        .unwrap();
        let admitted = admit_witness(&world, Some(&work));
        validate_intent(&world, &admitted.intent).unwrap();
        if profile_change {
            std::fs::write(
                work.join(".claude.json"),
                b"{\"oauthAccount\":{\"accountUuid\":\"second-principal\"}}",
            )
            .unwrap();
        } else {
            let mut roster = world.seat().roster(Cli::Claude).unwrap();
            roster
                .rows
                .iter_mut()
                .find(|row| row.label == "work")
                .unwrap()
                .identity_email = Some("new-owner@example.test".into());
            std::fs::write(
                world.cfg.join(Cli::Claude.accounts().roster_file),
                accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
            )
            .unwrap();
        }
        let before = witness_images(&world);
        assert!(validate_intent(&world, &admitted.intent).is_err());
        assert_eq!(witness_images(&world), before);
    }
}

#[test]
fn cold_witness_missing_unmanaged_profile_is_not_an_account_identity_grant() {
    let world = World::new("cold-witness-unmanaged");
    std::fs::create_dir_all(world.home.join(".claude")).unwrap();
    let admitted = admit_witness(&world, None);
    assert!(matches!(
        &admitted.intent,
        ExecutionIntent::Stored { owner: None, .. }
    ));
    assert_eq!(
        crate::harness::profile::default_identity_evidence(&world.home, "").unwrap(),
        crate::harness::profile::IdentityEvidence::Missing
    );
    let before = witness_images(&world);
    validate_intent(&world, &admitted.intent).unwrap();
    assert_eq!(witness_images(&world), before);
    // A new default row changes the exact previously unmanaged namespace.
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        "[[account]]\nlabel='new-owner'\nclass='dedicated'\n",
    )
    .unwrap();
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}

#[test]
fn cold_witness_unavailable_profiles_never_become_missing_evidence() {
    for kind in ["malformed", "nonfile", "dangling"] {
        if kind == "dangling" && !cfg!(unix) {
            continue;
        }
        let (world, work) = witness_world("cold-witness-unavailable");
        let initial = admit_witness(&world, Some(&work));
        validate_intent(&world, &initial.intent).unwrap();
        let profile = work.join(".claude.json");
        std::fs::remove_file(&profile).unwrap();
        match kind {
            "malformed" => std::fs::write(&profile, b"not JSON").unwrap(),
            "nonfile" => std::fs::create_dir(&profile).unwrap(),
            _ => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(work.join("absent-profile"), &profile).unwrap();
            }
        }
        assert!(crate::harness::profile::identity_evidence_in_home(&work, "").is_err());
        let before = witness_images(&world);
        assert!(
            super::super::cold_launch_admitted(
                &world.seat(),
                Cli::Claude,
                accounts::ProcessLogin {
                    login: Some(work.to_str().unwrap().into()),
                    default_home: true,
                    route: crate::harness::accounts::AuthRoute::Stored,
                },
                &NoUsage,
                Some(&world.exe)
            )
            .is_err()
        );
        // Initial admission may perform its established claim bookkeeping; no credentials or
        // roster are repaired from invalid evidence. Validation itself is checked above.
        assert_eq!(
            std::fs::read(world.cfg.join(Cli::Claude.accounts().roster_file)).ok(),
            before[3]
        );
    }
}

#[test]
fn cold_witness_refuses_pending_publication_without_recovering_it() {
    let (world, work) = witness_world("cold-witness-pending");
    let admitted = admit_witness(&world, Some(&work));
    validate_intent(&world, &admitted.intent).unwrap();
    super::recovery::pending_renewal(&world, Cli::Claude, &work, false);
    let before = witness_images(&world);
    assert!(
        before[4].is_some(),
        "the actual interrupted publication must exist"
    );
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(
        witness_images(&world),
        before,
        "validation must not replay the pending publication"
    );
}

#[test]
fn cold_witness_rejects_email_only_replacement_with_no_provider_principal() {
    let (world, work) = witness_world("cold-witness-email-only");
    let path = work.join(".claude.json");
    let write = |email: &str| {
        std::fs::write(
            &path,
            aterm_json::to_vec(&aterm_json::json!({"oauthAccount":{"emailAddress":email}}))
                .unwrap(),
        )
        .unwrap()
    };
    write("first@example.test");
    let admitted = admit_witness(&world, Some(&work));
    validate_intent(&world, &admitted.intent).unwrap();
    write("second@example.test");
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}

/// A Codex pool of `main`, this machine's own login, and `work`, on `main`; `work`'s directory.
fn pool(tag: &str) -> (World, PathBuf) {
    let world = World::new(tag);
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    (world, work)
}

/// A stand-in Codex login in `home` that names the provider account `account`.
fn login_naming(home: &Path, account: &str) {
    std::fs::create_dir_all(home).unwrap();
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "iat": 1,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": format!("user-of-{account}"), "chatgpt_account_id": account
        }
    }));
    std::fs::write(
        home.join("auth.json"),
        aterm_json::to_vec(&aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        }))
        .unwrap(),
    )
    .unwrap();
}

/// The cold admission of a relaunch whose restart asked for `wanted`, of a session that ran on
/// the login directory `ran` (`None`: this machine's own login) under `program`.
fn asked(
    world: &World,
    ran: Option<&Path>,
    wanted: &Wanted,
    read: ReadUsage<'_>,
    program: &Path,
) -> Result<ColdAsked, String> {
    let auth = ran.map(|dir| dir.join("auth.json").display().to_string());
    let env: Vec<(&str, &str)> = auth
        .iter()
        .map(|auth| ("CODEX_AUTH_PATH", auth.as_str()))
        .collect();
    super::super::cold_launch_env_asked(
        &world.seat(),
        Cli::Codex,
        &env,
        read,
        Some(program),
        Some(wanted),
    )
}

fn row(label: &str) -> Wanted {
    Wanted::Row(label.to_owned())
}

/// A session whose own row still serves is moved to the row its restart asked for once that row
/// answers a read, and to nothing the pool keeps: no pin, no active row, no rotation charged.
/// A later attempt of the same relaunch reads no provider and still takes the row while that
/// reading is fresh; once it is due, the row is unproven again and the session keeps its own.
#[test]
fn a_relaunch_starts_on_the_row_its_restart_asked_for_once_that_row_answers_a_read() {
    let (world, work) = pool("asked-moves");
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let before = world.load(Cli::Codex);
    let taken = asked(&world, None, &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(taken.kept, None);
    let pick = taken.pick.expect("it moves");
    assert_eq!(pick.label, "work");
    assert_eq!(pick.value.as_deref(), work.join("auth.json").to_str());
    assert_eq!(
        reads.homes.lock().unwrap().as_slice(),
        std::slice::from_ref(&work),
        "the asked row is the row the relaunch starts on: it alone is read"
    );
    let after = world.load(Cli::Codex);
    assert_eq!(
        (&after.active, after.pinned, &after.rotations),
        (&before.active, before.pinned, &before.rotations),
        "the move is the session's, never the pool's"
    );
    assert_eq!(after.active.as_deref(), Some("main"));
    let again = asked(&world, None, &row("work"), &NoUsage, &world.exe).unwrap();
    assert_eq!(
        again.pick.map(|pick| pick.label).as_deref(),
        Some("work"),
        "a reading taken a moment ago proves the row to an attempt that reads nothing"
    );
    let later = Seat {
        now: NOW + crate::harness::accounts_pool::USAGE_FRESH_S,
        ..world.seat()
    };
    let stale = super::super::cold_launch_env_asked(
        &later,
        Cli::Codex,
        &[],
        &NoUsage,
        Some(&world.exe),
        Some(&row("work")),
    )
    .unwrap();
    assert_eq!(stale.pick, None, "a reading that old proves nothing");
    assert_eq!(stale.kept.as_deref(), Some(super::super::MOVE_UNREAD));
    // With none asked the relaunch keeps its row, as before.
    let plain =
        super::super::cold_launch_env_asked(&world.seat(), Cli::Codex, &[], &NoUsage, None, None)
            .unwrap();
    assert_eq!((plain.pick, plain.kept), (None, None));
}

/// The machine's own login is asked for as any row: its relaunch names no login file.
#[test]
fn a_relaunch_that_asked_for_this_machines_own_login_names_no_login_file() {
    let (world, work) = pool("asked-default");
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let taken = asked(&world, Some(&work), &row("main"), &reads, &world.exe).unwrap();
    let pick = taken.pick.expect("it moves");
    assert_eq!((pick.label.as_str(), pick.value), ("main", None));
    assert_eq!(
        *reads.homes.lock().unwrap(),
        [world.home.join(Cli::Codex.accounts().default_home)]
    );
}

/// NEGATIVE CONTROL: a row never measured reads as fresh on record, so without the answered read
/// this relaunch would start on a row nothing vouches for.
#[test]
fn an_asked_row_that_answers_no_read_starts_nothing_and_the_session_keeps_its_row() {
    let (world, work) = pool("asked-unread");
    let answer = |home: &Path| {
        if home == work {
            Reading::Unread
        } else {
            plain_usage()
        }
    };
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let kept = asked(&world, None, &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(kept.pick, None, "it keeps the row it ran on");
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_UNREAD));
    assert_eq!(
        *reads.homes.lock().unwrap(),
        [
            work.clone(),
            world.home.join(Cli::Codex.accounts().default_home)
        ],
        "CONTROL: the asked row was read, then the row it ran on, as with none asked"
    );
    assert_eq!(world.load(Cli::Codex).account("work").sample, None);
}

/// A read that shows the asked row at its limit keeps the session where it was, and the wall the
/// read found stays on record for the next pick.
#[test]
fn an_asked_row_whose_read_shows_a_wall_starts_nothing_and_its_wall_is_recorded() {
    let (world, work) = pool("asked-walled-by-read");
    let answer = |home: &Path| {
        if home == work {
            spent_usage()
        } else {
            plain_usage()
        }
    };
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let kept = asked(&world, None, &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(kept.pick, None);
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_LIMITED));
    assert!(matches!(
        condition_of(&world, Cli::Codex, "work"),
        Condition::Limited { .. }
    ));
}

/// A relaunch whose own row stopped serving starts where a relaunch with none asked would. Where
/// that serving row is the asked one, its restart's ask was taken, though the asked row's own
/// read answered nothing: nothing says it was not.
#[test]
fn a_serving_pick_that_lands_on_the_asked_row_took_it() {
    let (world, work) = pool("asked-then-picked");
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    let read = std::sync::atomic::AtomicUsize::new(0);
    let answer = |home: &Path| {
        if home == work && read.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Reading::Unread
        } else {
            plain_usage()
        }
    };
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let moved = asked(&world, None, &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(
        moved.pick.map(|pick| pick.label).as_deref(),
        Some("work"),
        "CONTROL: the serving pick read it again and starts on it"
    );
    assert_eq!(
        moved.kept, None,
        "the session starts on the row it asked for"
    );
}

/// Each refusal on record keeps the session on its own row, never holds it down, and reads
/// nothing of the asked row.
#[test]
fn an_asked_row_that_does_not_serve_on_record_is_refused_by_its_word_and_never_read() {
    // Every case is judged before any fails the test, so one run names each word that is wrong.
    let wrong = std::cell::RefCell::new(Vec::new());
    let case = |tag: &str, wanted: Wanted, word: &str, arrange: &dyn Fn(&World, &Path)| {
        let (world, work) = pool(tag);
        arrange(&world, &work);
        let answer = |_: &Path| plain_usage();
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer: &answer,
        };
        // The restart act's judgement on record is the admission's: one word for both.
        let act = super::super::moves(&world.seat(), Cli::Codex, &[], Some(&world.exe))
            .map_or_else(Some, |moves| moves.refusal(&wanted));
        let kept = asked(&world, None, &wanted, &reads, &world.exe);
        let read = reads.homes.lock().unwrap().contains(&work.to_path_buf());
        let state = world.load(Cli::Codex);
        let pool = (state.active.as_deref(), state.pinned, state.rotations.len());
        let want = ColdAsked {
            pick: None,
            kept: Some(word.to_owned()),
        };
        if kept.as_ref() != Ok(&want)
            || read
            || pool != (Some("main"), false, 0)
            || act != Some(word)
        {
            wrong.borrow_mut().push(format!(
                "{tag}: {kept:?}, the act's word: {act:?}, asked row read: {read}, pool: {pool:?}"
            ));
        }
    };
    let rewrite = |world: &World, change: &dyn Fn(&mut Row)| {
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        change(
            roster
                .rows
                .iter_mut()
                .find(|row| row.label == "work")
                .unwrap(),
        );
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
    };
    let record = |world: &World, change: &dyn Fn(&mut State)| {
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| {
                change(state);
                Ok(Vec::new())
            })
            .unwrap();
    };
    case(
        "asked-unknown",
        row("nope"),
        super::super::MOVE_UNKNOWN,
        &|_, _| {},
    );
    case(
        "asked-current",
        row("main"),
        super::super::MOVE_CURRENT,
        &|_, _| {},
    );
    case(
        "asked-not-enrolled",
        row("work"),
        super::super::MOVE_NOT_ENROLLED,
        &|_, work| std::fs::remove_file(work.join("auth.json")).unwrap(),
    );
    case(
        "asked-signed-out",
        row("work"),
        super::super::MOVE_SIGNED_OUT,
        &|world, _| {
            world
                .seat()
                .store(Cli::Codex)
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(stage_current_auth(
                        world,
                        Cli::Codex,
                        state,
                        logins,
                        "work",
                        accounts::Auth::SignedOut,
                        NOW,
                    ))
                })
                .unwrap();
        },
    );
    case(
        "asked-limited",
        row("work"),
        super::super::MOVE_LIMITED,
        &|world, _| wall(world, Cli::Codex, "work", NOW + HOUR),
    );
    case(
        "asked-held",
        row("work"),
        super::super::MOVE_HELD,
        &|world, _| rewrite(world, &|row| row.held_by = Some("another-machine".into())),
    );
    case(
        "asked-leased",
        row("work"),
        super::super::MOVE_HELD,
        &|world, _| {
            record(world, &|state| {
                state.leases.insert(
                    "work".to_owned(),
                    accounts::AccountLease {
                        holder: "m9".to_owned(),
                        until: NOW + HOUR,
                        at: NOW - HOUR,
                    },
                );
            });
        },
    );
    case(
        "asked-identity",
        row("work"),
        super::super::MOVE_IDENTITY,
        &|world, work| {
            usage_policy::write_context_login(work, 1);
            rewrite(world, &|row| {
                row.email = Some("another@example.test".into())
            });
        },
    );
    case(
        "asked-same-account",
        row("work"),
        super::super::MOVE_SAME,
        &|world, work| {
            login_naming(work, "one-account");
            login_naming(
                &world.home.join(Cli::Codex.accounts().default_home),
                "one-account",
            );
        },
    );
    case(
        "asked-unanswered-run",
        row("work"),
        super::super::MOVE_UNREAD,
        &|world, _| {
            record(world, &|state| {
                accounts::note_unanswered(state, "work", NOW)
            })
        },
    );
    case(
        "asked-off",
        row("work"),
        super::super::MOVE_OFF,
        &|world, _| {
            aterm_uds::atomic_file::write(
                &world.cfg.join("aterm.toml"),
                b"[harness]\naccounts='off'\n",
                0o600,
            )
            .unwrap();
        },
    );
    let wrong = wrong.into_inner();
    assert!(wrong.is_empty(), "{wrong:#?}");
    // CONTROL for `account-same`: two logins naming two accounts are two rows to move between.
    let (world, work) = pool("asked-two-accounts");
    login_naming(&work, "one-account");
    login_naming(
        &world.home.join(Cli::Codex.accounts().default_home),
        "another-account",
    );
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let taken = asked(&world, None, &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(taken.pick.map(|pick| pick.label).as_deref(), Some("work"));
}

/// A program that keeps no row's login its own runs only on this machine's own login, and a
/// session whose login is no row of the pool is not the pool's to move.
#[test]
fn an_asked_row_is_not_taken_by_a_program_or_a_login_the_pool_does_not_manage() {
    let (world, work) = pool("asked-default-only");
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let old = crate::harness::codex_alab::lay_package(&world.root.join("old-program"), false);
    let kept = asked(&world, None, &row("work"), &reads, &old).unwrap();
    assert_eq!(kept.pick, None);
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_UNMANAGED));
    assert!(!reads.homes.lock().unwrap().contains(&work));
    let own = world.root.join("unmanaged");
    usage_policy::write_context_login(&own, 1);
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let kept = asked(&world, Some(&own), &row("work"), &reads, &world.exe).unwrap();
    assert_eq!(kept.pick, None);
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_UNMANAGED));
    assert!(reads.homes.lock().unwrap().is_empty(), "nothing is read");
}

/// A row whose login names another account neither takes a session nor hands one off: with the
/// session's own row contradicted, asking for another row changes nothing of what the relaunch
/// does (it is held, as with none asked) and reads nothing of the asked row.
#[test]
fn a_session_whose_own_login_names_another_account_is_not_handed_off_on_its_asking() {
    let arrange = || {
        let (world, work) = pool("asked-from-contradicted");
        usage_policy::write_context_login(&world.home.join(Cli::Codex.accounts().default_home), 1);
        let mut roster = world.seat().roster(Cli::Codex).unwrap();
        roster
            .rows
            .iter_mut()
            .find(|row| row.label == "main")
            .unwrap()
            .email = Some("another@example.test".into());
        std::fs::write(
            world.cfg.join(Cli::Codex.accounts().roster_file),
            accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
        )
        .unwrap();
        (world, work)
    };
    let (world, work) = arrange();
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let unasked = super::super::cold_launch_env(&world.seat(), Cli::Codex, &[], &reads, None);
    assert!(
        unasked.is_err(),
        "CONTROL: its relaunch is held: {unasked:?}"
    );
    let (world, work_again) = arrange();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let asked_for = asked(&world, None, &row("work"), &reads, &world.exe);
    assert!(asked_for.is_err(), "{asked_for:?}");
    assert!(!reads.homes.lock().unwrap().contains(&work_again));
    drop(work);
}

/// `account-auto` is one row, the one a launch would start on as the records stand, taken by the
/// same proof as a named row; it never goes through the launch's own pick, whose refusal would
/// hold the relaunch.
#[test]
fn a_relaunch_that_asked_for_the_pools_choice_starts_on_the_row_a_launch_would() {
    let (world, work) = pool("asked-pool");
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let taken = asked(&world, Some(&work), &Wanted::Pool, &reads, &world.exe).unwrap();
    let pick = taken.pick.expect("it moves to the pool's row");
    assert_eq!((pick.label.as_str(), pick.value), ("main", None));
    let kept = asked(&world, None, &Wanted::Pool, &reads, &world.exe).unwrap();
    assert_eq!(kept.pick, None, "the pool's choice is the row it ran on");
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_CURRENT));
    let nobody = std::collections::BTreeSet::new();
    let snapshot = world.load(Cli::Codex);
    let roster = world.seat().bound_roster(Cli::Codex).unwrap();
    assert_eq!(
        super::super::asked_row(&roster, &snapshot, &nobody, &Wanted::Pool, NOW).map(|_| ()),
        Err(super::super::MOVE_NO_CHOICE),
        "a pool with no login here has no choice"
    );
}

/// CONTROL: a launch from this pool is refused, since its pin holds a row whose login names
/// another account, so a relaunch handed to the launch's own pick would be held. Asked for by a
/// session on another row, the pool's choice is judged as any asked row is and refused by its
/// word: the session keeps the row it ran on, and the pin stands.
#[test]
fn a_pools_choice_that_cannot_serve_keeps_the_session_on_the_row_it_ran_on() {
    let (world, work) = pool("asked-pool-pinned-contradicted");
    usage_policy::write_context_login(&world.home.join(Cli::Codex.accounts().default_home), 1);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    roster
        .rows
        .iter_mut()
        .find(|row| row.label == "main")
        .unwrap()
        .email = Some("another@example.test".into());
    std::fs::write(
        world.cfg.join(Cli::Codex.accounts().roster_file),
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let answer = |_: &Path| plain_usage();
    let reads = || Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let launch = super::super::launch_env(&world.seat(), Cli::Codex, &reads());
    assert!(
        launch.is_err(),
        "CONTROL: the launch pick refuses: {launch:?}"
    );
    let kept = asked(&world, Some(&work), &Wanted::Pool, &reads(), &world.exe).unwrap();
    assert_eq!(kept.pick, None, "it keeps the row it ran on");
    assert_eq!(kept.kept.as_deref(), Some(super::super::MOVE_IDENTITY));
    assert!(world.load(Cli::Codex).pinned, "the pin stands");
}

/// The asked row's read is one of the launch's [`LAUNCH_READS`]: where every row reads spent, the
/// asked row, the row it ran on and the serving row's picks are read that many times in all.
#[test]
fn an_asked_rows_read_counts_against_a_launchs_cap() {
    let world = usage_policy::six("asked-cap");
    let dir = |label: &str| world.root.join("logins").join("codex").join(label);
    let answer = |_: &Path| spent_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let _ = asked(&world, Some(&dir("w2")), &row("w3"), &reads, &world.exe);
    let homes = reads.homes.lock().unwrap();
    assert_eq!(
        homes.len(),
        crate::harness::accounts_pool::LAUNCH_READS,
        "{homes:?}"
    );
    assert_eq!(homes[..2], [dir("w3"), dir("w2")], "asked, then ran on");
}

/// A ROW'S CONDITION AND A MOVE TO IT ARE READ OFF ONE READING ([`move_records`]): a sign-out
/// recorded under the login the row's directory holds is that row's, so the row reads signed out
/// and a move to it is refused by that word. CONTROL: the records read raw project no sign-out,
/// so a report that read its conditions off them would call the row it refuses unseen.
#[test]
fn a_rows_condition_and_a_move_to_it_are_judged_on_one_reading() {
    let (world, _work) = pool("moves-one-reading");
    world
        .seat()
        .store(Cli::Codex)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(stage_current_auth(
                &world,
                Cli::Codex,
                state,
                logins,
                "work",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let seat = world.seat();
    let condition_in = |records: &accounts::AccountRecords| {
        let enrolled = accounts::enrolled_in(Cli::Codex, &records.roster, &world.home);
        accounts::condition(
            records.roster.row("work").unwrap(),
            &records.state,
            enrolled.contains("work"),
            NOW,
        )
    };
    let records = super::super::move_records(&seat, Cli::Codex).unwrap();
    assert_eq!(condition_in(&records), Condition::SignedOut);
    let moves = super::super::moves_in(&seat, Cli::Codex, &records, &[], Some(&world.exe)).unwrap();
    assert_eq!(
        moves.refusal(&row("work")),
        Some(super::super::MOVE_SIGNED_OUT)
    );
    let raw = seat
        .store(Cli::Codex)
        .peek_account_records(&world.cfg.join(Cli::Codex.accounts().roster_file), NOW)
        .unwrap();
    assert_ne!(
        condition_in(&raw),
        Condition::SignedOut,
        "CONTROL: the raw records project no sign-out"
    );
}

/// WHAT A RUNNING SESSION MAY ASK THE POOL FOR ([`moves`]): the row it runs on is the one its
/// launch's login names, the pool's choice is the row a launch would get, and each row carries
/// the word a move to it is refused by. A session on `main` may ask for `work` and not for the
/// pool's choice, which is its own row; one on `work` may ask for the pool's choice and not for
/// `work`. No session is judged where the pool moves none: a login kept in the keyring, a login
/// that is no row of the pool, another CLI; and records that cannot be read say so.
#[test]
fn the_pool_says_what_a_running_session_may_ask_for() {
    let (world, work) = pool("moves");
    let pool_word = |env: &[(&str, &str)], wanted: &Wanted| {
        super::super::moves(&world.seat(), Cli::Codex, env, Some(&world.exe))
            .map(|moves| moves.refusal(wanted))
    };
    let on_main = super::super::moves(&world.seat(), Cli::Codex, &[], Some(&world.exe)).unwrap();
    assert_eq!(
        on_main,
        Moves {
            ran: "main".into(),
            choice: Some("main".into()),
            pinned: false,
            mode: Mode::Early,
            own_login: true,
            rows: vec![
                ("main".into(), Some(super::super::MOVE_CURRENT)),
                ("work".into(), None),
            ],
        }
    );
    assert_eq!(on_main.refusal(&row("work")), None);
    assert_eq!(
        on_main.refusal(&row("nope")),
        Some(super::super::MOVE_UNKNOWN)
    );
    assert_eq!(
        on_main.refusal(&Wanted::Pool),
        Some(super::super::MOVE_CURRENT),
        "the pool's choice is the row it runs on"
    );
    let auth = work.join("auth.json").display().to_string();
    let on_work = [("CODEX_AUTH_PATH", auth.as_str())];
    assert_eq!(pool_word(&on_work, &Wanted::Pool), Ok(None));
    assert_eq!(
        pool_word(&on_work, &row("work")),
        Ok(Some(super::super::MOVE_CURRENT))
    );
    let own = world.root.join("unmanaged");
    usage_policy::write_context_login(&own, 1);
    let auth = own.join("auth.json").display().to_string();
    assert_eq!(
        pool_word(&[("CODEX_AUTH_PATH", auth.as_str())], &row("work")),
        Err(super::super::MOVE_UNMANAGED),
        "a login that is no row of the pool"
    );
    {
        // A login its row moved off: a renewal bound `work` to a new directory, and a session
        // still runs on the old one, which no row holds now.
        let (world, work) = pool("moves-left");
        let renewed = world.root.join("renewed");
        usage_policy::write_context_login(&renewed, 2);
        world
            .seat()
            .store(Cli::Codex)
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    work.to_str(),
                    renewed.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
        let moves = |auth: &Path| {
            let auth = auth.join("auth.json").display().to_string();
            super::super::moves(
                &world.seat(),
                Cli::Codex,
                &[("CODEX_AUTH_PATH", auth.as_str())],
                Some(&world.exe),
            )
            .map(|moves| moves.ran)
        };
        assert_eq!(
            moves(&renewed).as_deref(),
            Ok("work"),
            "CONTROL: the row's new login is the row's"
        );
        assert_eq!(
            moves(&work),
            Err(super::super::MOVE_UNMANAGED),
            "the login it moved off is no row's own"
        );
    }
    assert_eq!(
        super::super::moves(&world.seat(), Cli::Claude, &[], None).map(|moves| moves.ran),
        Err(super::super::MOVE_UNMANAGED),
        "another CLI's session"
    );
    let roster = world.cfg.join(Cli::Codex.accounts().roster_file);
    let listed = std::fs::read_to_string(&roster).unwrap();
    std::fs::write(&roster, "not a roster [").unwrap();
    assert_eq!(
        pool_word(&[], &row("work")),
        Err(super::super::MOVE_UNREAD),
        "records that cannot be read"
    );
    std::fs::write(&roster, listed).unwrap();
    assert_eq!(pool_word(&[], &row("work")), Ok(None), "CONTROL: mended");
    assert!(on_main.offered(), "CONTROL: this session can be moved");
    let old = crate::harness::codex_alab::lay_package(&world.root.join("old-program"), false);
    let on_old = super::super::moves(&world.seat(), Cli::Codex, &[], Some(&old)).unwrap();
    assert!(
        !on_old.own_login && !on_old.offered(),
        "a program that keeps no row's login its own is moved nowhere: {on_old:?}"
    );
    assert_eq!(
        on_old.refusal(&row("work")),
        Some(super::super::MOVE_UNMANAGED)
    );
    // Its launch named `work`, but it runs on this machine's own login whatever its launch named:
    // no row is its own, so not even this machine's own row is a move it may ask for.
    let on_old_work = super::super::moves(&world.seat(), Cli::Codex, &on_work, Some(&old)).unwrap();
    assert_eq!(on_old_work.ran, "work", "CONTROL: its launch named `work`");
    assert_eq!(
        on_old_work.refusal(&row("main")),
        Some(super::super::MOVE_UNMANAGED),
        "the act refuses what the snapshot's `select` does not offer"
    );
    assert!(!on_old_work.offered());
    assert!(
        !Moves {
            mode: Mode::Off,
            ..on_main.clone()
        }
        .offered(),
        "rotation off moves no session"
    );
    assert_eq!(
        Moves {
            choice: None,
            ..on_main.clone()
        }
        .refusal(&Wanted::Pool),
        Some(super::super::MOVE_NO_CHOICE),
        "a pool with no row to launch on"
    );
    assert_eq!(
        Moves {
            mode: Mode::Off,
            ..on_main
        }
        .refusal(&row("work")),
        Some(super::super::MOVE_OFF),
        "rotation off moves no session"
    );
    std::fs::write(
        world
            .home
            .join(Cli::Codex.accounts().default_home)
            .join("config.toml"),
        "cli_auth_credentials_store='keyring'\n",
    )
    .unwrap();
    assert_eq!(
        pool_word(&[], &row("work")),
        Err(super::super::MOVE_UNMANAGED),
        "a login kept in the keyring"
    );
}
