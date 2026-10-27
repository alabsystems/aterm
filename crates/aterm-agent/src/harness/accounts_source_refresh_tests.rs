// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::login_tests::{Desk, NOW, upkeep_effects};
use super::*;
use aterm_spec::interp::State;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const FIRST: &str = "[[account]]\nlabel='m27'\nmachines=['m27']\nemail='sol-m27@example.test'\n";
const SECOND: &str = concat!(
    "[[account]]\nlabel='m27'\nmachines=['m27']\nemail='sol-m27@example.test'\n",
    "[[account]]\nlabel='m27-2'\nmachines=['m27']\nemail='sun-m27@example.test'\n"
);

fn write_same_stamp(path: &Path, text: &str) {
    assert!(text.len() <= SECOND.len());
    fs::write(path, format!("{text:width$}", width = SECOND.len())).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000))
        .unwrap();
}

fn local_source_desk() -> (Desk, PathBuf) {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    let source = desk.config.join("team-fleet.toml");
    write_same_stamp(&source, FIRST);
    fs::write(
        desk.config.join("aterm.toml"),
        format!(
            "[fleet]\nmachine='m27'\naccounts_source={}\n",
            aterm_toml::Value::from(source.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    desk.store()
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "m27",
                "seven_day",
                Some(NOW + 86_400),
                NOW,
                NOW,
            ))
        })
        .unwrap();
    (desk, source)
}

fn records(desk: &Desk) -> Vec<Option<Vec<u8>>> {
    let store = desk.store();
    [
        desk.roster(),
        desk.fleet_file(),
        store.state_file(),
        store.state_witness_file(),
        store.logins_file(),
        store.logins_witness_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .iter()
    .map(|path| fs::read(path).ok())
    .collect()
}

/// [`REDISCOVER_EVERY`] passes: for the source's last attempt and the last failed sync's alike.
fn expire(rediscovery: &Mutex<Rediscovery>) {
    let mut held = rediscovery.lock().unwrap();
    held.key_attempt.as_mut().unwrap().at = Instant::now() - REDISCOVER_EVERY;
    if let Some(failed) = held.failed.as_mut() {
        failed.at = Instant::now() - REDISCOVER_EVERY;
    }
}

/// A disappearing source must report failure through setup and window upkeep while retaining
/// account, login, usage and wake records. Repeated polls before the deadline stay quiet.
#[test]
fn a_missing_configured_source_reports_failure_and_keeps_the_roster() {
    for window in [false, true] {
        let (desk, source) = local_source_desk();
        let before = records(&desk);
        let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let mut fx = upkeep_effects(&fetch_source_text, &spawn, &cooldown, &rediscovery, &guard);
        let logged = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&logged);
        fx.log = Arc::new(move |line| into.lock().unwrap().push(line.to_owned()));
        let pass = || resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
        assert!(pass().synced);
        fs::rename(&source, source.with_extension("saved")).unwrap();
        if window {
            upkeep_claude(
                &desk.home,
                &desk.state,
                (&desk.roster(), NOW, UpkeepStep::Sync),
                &fx,
            );
            let log = logged.lock().unwrap();
            assert_eq!(log.len(), 1);
            assert!(log[0].contains("could not read"), "{log:?}");
        } else {
            let missing = pass();
            assert!(
                missing.failed.is_some(),
                "missing source must be reported: {missing:?}"
            );
        }
        assert_eq!(records(&desk), before);
        assert_eq!(
            pass(),
            Resync::default(),
            "same missing source is throttled"
        );
        expire(&rediscovery);
        assert!(pass().failed.is_some(), "missing source is retried");
        assert_eq!(records(&desk), before);
        fs::rename(source.with_extension("saved"), source).unwrap();
        let recovered = pass();
        assert!(
            recovered.synced && recovered.failed.is_none(),
            "{recovered:?}"
        );
        assert_eq!(records(&desk), before, "unchanged recovery writes nothing");
    }
}

/// A failed fetch is an attempt, not a successful observation. Only monotonic elapsed time
/// permits another unchanged attempt; wall-clock jumps cannot bypass or prevent that retry.
#[test]
fn a_transient_refresh_failure_retries_with_unchanged_source_metadata() {
    let (desk, source) = local_source_desk();
    write_same_stamp(&source, SECOND);
    let before = records(&desk);
    let attempts = AtomicUsize::new(0);
    let fetch = |env: &Env, source: &str| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("transient read failure".to_owned())
        } else {
            fetch_source_text(env, source)
        }
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let pass = |now| resync_with(&desk.home, &desk.state, (&desk.roster(), now, false), &fx);
    assert!(pass(NOW).failed.is_some());
    assert_eq!(records(&desk), before);
    assert!(!pass(NOW + 365 * 86400).synced);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    expire(&rediscovery);
    let recovered = pass(NOW - 365 * 86400);
    assert!(
        recovered.synced && recovered.changed && recovered.failed.is_none(),
        "{recovered:?}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(desk.rows().rows.len(), 2);
}

/// Equal mtimes and lengths are hints, never proof of equal content. Invalid edits retain
/// records; repairing those bytes with the same metadata is found on revalidation as well.
#[test]
fn same_stamp_edits_and_repairs_are_eventually_read() {
    let (desk, source) = local_source_desk();
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch_source_text, &spawn, &cooldown, &rediscovery, &guard);
    let pass = || resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    assert!(pass().synced);
    let before = records(&desk);
    write_same_stamp(&source, "[[account]\n");
    assert_eq!(pass(), Resync::default());
    expire(&rediscovery);
    assert!(pass().failed.is_some());
    assert_eq!(records(&desk), before);
    write_same_stamp(&source, SECOND);
    assert_eq!(pass(), Resync::default());
    expire(&rediscovery);
    let recovered = pass();
    assert!(
        recovered.synced && recovered.changed && recovered.failed.is_none(),
        "{recovered:?}"
    );
    assert_eq!(desk.rows().rows.len(), 2);
}

#[test]
fn local_observations_track_path_and_length_without_polling_urls() {
    let (desk, source) = local_source_desk();
    let config = desk.config.join("aterm.toml");
    let rediscovery = Mutex::new(Rediscovery::default());
    let due = || key_file_changed(&desk.home, &desk.state, &config, NOW, &rediscovery);
    assert!(due());
    assert!(!due());
    let file = fs::File::options().write(true).open(&source).unwrap();
    file.set_len((SECOND.len() + 1) as u64).unwrap();
    file.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000))
        .unwrap();
    assert!(due(), "size changed at the same mtime");
    let other = desk.config.join("other.toml");
    fs::copy(&source, &other).unwrap();
    let configure = |source: &str| {
        fs::write(
            &config,
            format!(
                "[fleet]\nmachine='m27'\naccounts_source={}\n",
                aterm_toml::Value::from(source)
            ),
        )
        .unwrap();
    };
    configure(other.to_str().unwrap());
    assert!(due(), "source path changed");
    expire(&rediscovery);
    configure("https://example.test/fleet.csv");
    assert!(!due(), "this periodic trigger never polls a URL");
    configure(other.to_str().unwrap());
    assert!(
        due(),
        "returning from a URL invalidates the local observation"
    );
    fs::write(&config, "[fleet]\nmachine='m27'\n").unwrap();
    assert!(
        !sync_called_for(
            &desk.home,
            &desk.state,
            (&desk.roster(), &config),
            NOW,
            &rediscovery
        ),
        "no key, no periodic fetch"
    );
    configure(other.to_str().unwrap());
    assert!(
        due(),
        "returning from manual mode invalidates the local observation"
    );
}

/// A legitimately empty fleet has long meant no assignments here. Reading one may update
/// the cached fleet, but never clears retained rows, login bindings, usage or ledger entries.
#[test]
fn an_empty_source_keeps_existing_account_authority() {
    let (desk, source) = local_source_desk();
    let before = records(&desk);
    fs::write(source, "").unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    let after = records(&desk);
    for index in [0, 2, 3, 4, 5, 6] {
        assert!(
            before[index].is_some(),
            "fixture creates account record {index}"
        );
        assert_eq!(
            before[index], after[index],
            "retained account record {index}"
        );
    }
}

/// Explicit paths retain symlink support but may only read bounded UTF-8 regular files.
#[test]
fn explicit_local_sources_reject_nonregular_and_unbounded_inputs() {
    let (desk, _) = local_source_desk();
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    let path = desk.config.join("input");
    let read = || fetch_source_text(&env, path.to_str().unwrap());
    assert!(read().is_err(), "missing");
    fs::create_dir(&path).unwrap();
    assert!(read().unwrap_err().contains("not a regular file"));
    fs::rename(&path, desk.config.join("saved-dir")).unwrap();
    fs::write(&path, [0xff]).unwrap();
    assert!(read().unwrap_err().contains("not valid UTF-8"));
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(SOURCE_TEXT_MAX_BYTES as u64 + 1)
        .unwrap();
    assert!(read().unwrap_err().contains("byte limit"));
    fs::write(&path, FIRST).unwrap();
    assert_eq!(read().unwrap(), FIRST);
    #[cfg(unix)]
    {
        let link = desk.config.join("linked-input");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            fetch_source_text(&env, link.to_str().unwrap()).unwrap(),
            FIRST
        );
    }
}

#[cfg(unix)]
#[test]
fn explicit_local_fifos_are_refused_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt as _;
    let (desk, _) = local_source_desk();
    let fifo = desk.config.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated fixture path lives through mkfifo and names no owner file.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let link = desk.config.join("linked-fifo");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    let (send, recv) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        let answers: Vec<_> = [fifo, link]
            .iter()
            .map(|path| fetch_source_text(&env, path.to_str().unwrap()))
            .collect();
        send.send(answers).unwrap();
    });
    let answers = recv
        .recv_timeout(Duration::from_secs(60))
        .expect("source read hung waiting for a FIFO writer");
    worker.join().unwrap();
    for answer in answers {
        assert!(answer.unwrap_err().contains("not a regular file"));
    }
}

fn version(source: i64) -> LocalSourceVersion {
    (
        PathBuf::from("fixture"),
        (source > 0).then(|| {
            (
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(source as u64),
                10,
            )
        }),
    )
}

pub(crate) fn refresh_transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Edit" => {
            next.insert("source", if state["source"] == 1 { 2 } else { 1 });
            next.insert("changes", state["changes"] + 1);
            next.insert("checked", 0);
        }
        "Disappear" => {
            next.insert("source", 0);
            next.insert("changes", state["changes"] + 1);
            next.insert("checked", 0);
        }
        "Elapse" => {
            next.insert("age", state["age"] + 1);
            next.insert("checked", 0);
        }
        "Poll" => {
            let now = Instant::now() + REDISCOVER_EVERY;
            let half = REDISCOVER_EVERY / 2;
            let mut last = (state["seen"] < 3).then(|| LocalSourceAttempt {
                version: version(state["seen"]),
                at: now - half * state["age"] as u32,
            });
            let due = schedule_local_source(&mut last, version(state["source"]), now);
            next.insert("before_seen", state["seen"]);
            next.insert("before_age", state["age"]);
            next.insert(
                "required",
                i64::from(state["seen"] != state["source"] || state["age"] == 2),
            );
            next.insert("due", i64::from(due));
            next.insert(
                "seen",
                last.as_ref().map_or(3, |last| {
                    last.version.1.map_or(0, |(time, _)| {
                        time.duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64
                    })
                }),
            );
            next.insert(
                "age",
                last.map_or(state["age"], |last| {
                    (now.duration_since(last.at).as_secs() / half.as_secs()) as i64
                }),
            );
            next.insert("polls", state["polls"] + 1);
            next.insert("checked", 1);
        }
        _ => panic!("unknown source refresh action"),
    }
    next
}

#[test]
fn source_refresh_conforms_through_changes_quiet_polls_and_deadlines() {
    let model = aterm_models_harness::account_source_refresh_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut controls = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = refresh_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {} {next:?}",
                    action.name
                );
                if action.name == "Poll" {
                    controls.insert((next["due"], next["before_age"]), (state.clone(), next));
                }
            }
        }
    }
    for (key, field, value, invariant) in [
        ((1, 2), "due", 0, "DeadlineRetry"),
        ((0, 1), "due", 1, "QuietSuppressed"),
        ((0, 1), "age", 0, "QuietKeepsDeadline"),
        ((0, 1), "seen", 3, "QuietKeepsObservation"),
        ((1, 2), "seen", 3, "AttemptObserved"),
    ] {
        let (before, correct) = &controls[&key];
        let mut wrong = correct.clone();
        wrong.insert(field, value);
        assert!(!model.check_invariant(invariant, &wrong), "{invariant}");
        assert!(!model.successors("Poll", before).contains(&wrong));
    }
}
