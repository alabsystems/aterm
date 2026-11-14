// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Claude account login lifecycle core.

use super::tests::record_usage;
use super::*;
use crate::harness::host_path::abs;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

fn tmp(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-harness-logins-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn roster_text() -> String {
    format!(
        r#"
[[account]]
label = "main"
class = "dedicated"
email = "owner@example.com"
browser = "Claude main"

[[account]]
label = "work-2"
class = "dedicated"
dir = "{}"
"#,
        abs("/logins/work-2")
    )
}

fn roster() -> Roster {
    Roster::parse(&roster_text()).expect("admits")
}

fn admitted_observation(
    logins: &mut Logins,
    label: &str,
    at: i64,
    observe: impl FnOnce(&mut Logins) -> Vec<Event>,
) -> Vec<Event> {
    let home = tmp("admitted-observation");
    let store = Store::at(home.path());
    let path = home.path().join("accounts.toml");
    std::fs::write(&path, roster_text()).unwrap();
    store.save_logins(logins).unwrap();
    let mut observed = Vec::new();
    store
        .transact_pool_logins_within(&[], &path, at, &mut None, |raw, _, logins| {
            assert_eq!(raw, &roster());
            admit_workflow(logins, label)?;
            observed = observe(logins);
            Ok(observed.clone())
        })
        .unwrap();
    *logins = store.load_logins(at).unwrap().0;
    assert!(current_workflow(roster().row(label).unwrap(), logins).is_some());
    observed
}

/// No row's account is another Mac's ([`renew_due`]'s `others`).
const NONE: OtherMacs = OtherMacs::NONE;

fn every(roster: &Roster) -> BTreeSet<String> {
    roster.rows.iter().map(|r| r.label.clone()).collect()
}

fn state() -> State {
    State {
        machine: "0a0b".to_string(),
        ..State::default()
    }
}

fn renewed(at: i64) -> (State, Logins) {
    let mut state = state();
    let mut logins = Logins::default();
    note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        &abs("/s/main.1"),
        Some("max"),
        at,
    );
    // A session here ran on it and has ended: this Mac uses the login ([`used_here`]).
    let _ = note_session(&mut state, "s-main", "main", None, at);
    state.sessions.clear();
    (state, logins)
}

#[test]
fn an_email_and_a_browser_are_admitted_and_a_bad_one_refuses_the_file_unquoted() {
    let r = roster();
    assert_eq!(r.rows[0].email.as_deref(), Some("owner@example.com"));
    assert_eq!(r.rows[0].browser.as_deref(), Some("Claude main"));
    assert_eq!(
        (r.rows[1].email.clone(), r.rows[1].browser.clone()),
        (None, None)
    );
    let v1 =
        Roster::parse("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n").expect("admits");
    assert_eq!(
        v1.rows,
        vec![Row {
            label: "main".into(),
            ..Row::default()
        }]
    );
    let with = |key: &str, value: &str| {
        format!("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n{key} = \"{value}\"\n")
    };
    for bad in [
        "no-at",
        "a@b@example.com",
        "a b@example.com",
        "@example.com",
        "owner@",
        "owner@exa\tmple.com",
        &format!("{}@example.com", "o".repeat(250)),
    ] {
        let why = Roster::parse(&with("email", bad)).expect_err(bad);
        assert!(why.contains("row `main`: its email"), "{why}");
        assert!(!why.contains(bad), "the address is never quoted: {why}");
    }
    for bad in ["", "   ", "a\\u0007b", &"p".repeat(65)] {
        let why = Roster::parse(&with("browser", bad)).expect_err(bad);
        assert!(why.contains("its browser"), "{why}");
    }
    let why = Roster::parse("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = 1\n")
        .expect_err("a number");
    assert!(why.contains("its email is a TOML integer"), "{why}");
    let twice = format!(
        "{}\n[[account]]\nlabel = \"two\"\nclass = \"shared\"\ndir = \"{}\"\nemail = \"OWNER@example.com\"\n",
        with("email", "owner@example.com"),
        abs("/l/two")
    );
    let why = Roster::parse(&twice).expect_err("one email twice");
    assert!(
        why.contains("rows `main` and `two` name one email"),
        "{why}"
    );
    assert!(!why.to_lowercase().contains("owner@"), "{why}");
    let why = Roster::parse(&with("phone", "x")).expect_err("unknown");
    assert!(why.contains("unknown key `phone`"), "{why}");
}

#[test]
fn a_row_with_an_email_is_written_as_it_reads_back_and_never_printed() {
    let row = Row {
        label: "main".into(),
        email: Some("owner@example.com".into()),
        browser: Some("Claude \"main\"".into()),
        ..Row::default()
    };
    let text = append_rows("", std::slice::from_ref(&row)).expect("admits");
    assert_eq!(
        Roster::parse(&text).expect("reads back").rows,
        vec![row.clone()]
    );
    let inline = format!(
        "account = [ {{ label = \"w\", class = \"shared\", dir = \"{}\" }} ]\n",
        abs("/l/w")
    );
    let why = append_rows(&inline, std::slice::from_ref(&row)).expect_err("inline");
    assert!(why.contains(EMAIL_BY_HAND), "{why}");
    assert!(!why.contains("owner@example.com"), "{why}");
    let bad = Row {
        email: Some("owner".into()),
        ..row
    };
    let why = append_rows("", &[bad]).expect_err("no @");
    assert_eq!(
        why,
        "row `main`: its email is not one address (`name@domain`)"
    );
}

#[test]
fn logins_round_trip_past_the_roster_limit_and_beside_an_untouched_state() {
    let (_, mut logins) = renewed(NOW - DAY);
    note_login_notice(&mut logins, "main", 2, NOW - HOUR);
    note_served_login(&mut logins, "main", NOW - HOUR);
    note_wrong_account(&mut logins, "work-2", NOW);
    note_sign_in_task(&mut logins, "main", SignInAction::Browser, NOW);
    let text = logins.to_json();
    assert_eq!(Logins::from_json(&text, NOW), Ok(logins.clone()));

    let mut labels = Map::new();
    for i in 0..96 {
        labels.insert(
            format!("l{i:02}"),
            aterm_json::from_str(&format!(
                r#"{{"dir":"{}","renewed_at":9999999999,"lifetime_s":99999999,
                    "was":["{}","{}","{}",null],"subscription":"max"}}"#,
                abs("/s/current"),
                abs("/a"),
                abs("/b"),
                abs("/c")
            ))
            .expect("json"),
        );
    }
    let mut doc = Map::new();
    doc.insert("schema".into(), Value::from(1u64));
    doc.insert("labels".into(), Value::Object(labels));
    doc.insert("newer".into(), Value::from(true));
    let read = Logins::from_json(
        &aterm_json::to_string(&Value::Object(doc.clone())).expect("json"),
        NOW,
    )
    .expect("ours");
    assert_eq!(read.labels.len(), 96);
    let b = read.labels.values().next().expect("one");
    assert_eq!(b.dir.as_deref(), Some(abs("/s/current").as_str()));
    assert_eq!(b.renewed_at, Some(NOW), "no stamp from the future");
    assert_eq!(b.lifetime_s, LIFETIME_MAX_S);
    assert_eq!(b.was.len(), MAX_LEFT);
    let entries = doc.get_mut("labels").unwrap().as_object_mut().unwrap();
    entries.remove("l00");
    entries.insert("Bad Label".into(), Value::Object(Map::new()));
    assert!(Logins::from_json(&aterm_json::to_string(&doc).unwrap(), NOW).is_err());
    assert!(matches!(
        Logins::from_json("{\"schema\":2}", NOW),
        Err(Unread::NotOurs(_))
    ));
    assert!(matches!(
        Logins::from_json("{\"sch", NOW),
        Err(Unread::Torn(_))
    ));

    let dir = tmp("store");
    let store = Store::at(dir.path());
    let mut note = None;
    store
        .transact(NOW, &mut note, |s| Ok(note_auth(s, "main", Auth::Ok, NOW)))
        .expect("state");
    let before = std::fs::read(store.state_file()).expect("state.json");
    store
        .transact_logins(NOW, &mut note, |_, logins| {
            note_login_notice(logins, "main", 3, NOW);
            Ok(Vec::new())
        })
        .expect("logins");
    assert_eq!(
        std::fs::read(store.state_file()).expect("state.json"),
        before
    );
    assert!(!String::from_utf8_lossy(&before).contains("notice_end"));
    let (read, _) = store.load_logins(NOW).expect("reads");
    assert_eq!(
        read.get("main").and_then(|b| b.notice_end),
        Some(NOW + 3 * DAY)
    );
    std::fs::write(store.logins_file(), "{\"schema\":7}").expect("write");
    let refused = store.transact_logins(NOW, &mut note, |_, logins| {
        note_served_login(logins, "main", NOW);
        Ok(Vec::new())
    });
    assert!(refused.is_err());
    assert_eq!(
        std::fs::read_to_string(store.logins_file()).expect("kept"),
        "{\"schema\":7}"
    );
}

#[test]
fn a_sign_in_task_survives_restart_and_old_login_use_until_a_new_login_is_bound() {
    let tmp = tmp("sign-in-task");
    let store = Store::at(tmp.path());
    let (mut state, mut logins) = renewed(NOW - DAY);
    let dir = logins.labels["main"].dir.clone();
    note_sign_in_task(&mut logins, "main", SignInAction::Browser, NOW - HOUR);
    assert!(
        sign_in_tasks(&roster(), &logins).is_empty(),
        "unadmitted history is not a current step"
    );
    admitted_observation(&mut logins, "main", NOW - HOUR, |logins| {
        note_sign_in_task(logins, "main", SignInAction::Browser, NOW - HOUR);
        Vec::new()
    });
    store.save_logins(&logins).expect("save");
    let reopened = Store::at(tmp.path());
    let (mut logins, _) = reopened.load_logins(NOW).expect("reload");
    assert_eq!(
        sign_in_tasks(&roster(), &logins),
        [("main".into(), SignInAction::Browser)]
    );
    assert_eq!(logins.labels["main"].dir, dir);
    note_served_login(&mut logins, "main", NOW);
    assert!(logins.labels["main"].sign_in_task.is_some());
    note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        &abs("/s/main.2"),
        None,
        NOW,
    );
    assert!(sign_in_tasks(&roster(), &logins).is_empty());
    note_sign_in_task(&mut logins, "main", SignInAction::Login, NOW);
    note_wrong_account(&mut logins, "main", NOW);
    assert!(sign_in_tasks(&roster(), &logins).is_empty());
}

#[test]
fn a_renewal_supersedes_the_login_it_replaces_and_wakes_the_waiting() {
    let mut state = state();
    let mut logins = Logins::default();
    let events = note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        "/s/main.1",
        Some("max"),
        NOW,
    );
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Auth, Kind::Renewed]
    );
    assert!(events.iter().all(Event::wakes));
    assert_eq!(state.account("main").auth, Auth::Ok);
    let b = logins.get("main").expect("bound").clone();
    assert_eq!(b.dir.as_deref(), Some("/s/main.1"));
    assert_eq!(b.was, vec![None], "the default login, superseded");
    assert_eq!(b.subscription.as_deref(), Some("max"));
    note_login_notice(&mut logins, "main", 2, NOW + DAY);
    note_wrong_account(&mut logins, "main", NOW + DAY);
    for n in 2..=7 {
        note_renewed(
            &mut state,
            &mut logins,
            "main",
            None,
            &format!("/s/main.{n}"),
            None,
            NOW + n * DAY,
        );
    }
    let b = logins.get("main").expect("bound");
    assert_eq!(b.dir.as_deref(), Some("/s/main.7"));
    assert_eq!(
        b.was,
        ["/s/main.6", "/s/main.5", "/s/main.4", "/s/main.3"].map(|d| Some(d.to_string()))
    );
    assert_eq!(
        (b.notice_end, b.wrong_at, b.subscription.clone()),
        (None, None, None)
    );
    let mut logins = Logins::default();
    note_renewed(
        &mut state,
        &mut logins,
        "work-2",
        Some("/logins/work-2"),
        "/s/w.1",
        None,
        NOW,
    );
    assert_eq!(
        logins.get("work-2").expect("bound").was,
        vec![Some("/logins/work-2".to_string())]
    );
    let dir = tmp("staging");
    let store = Store::at(dir.path());
    let first = store.claim_staging_dir("main").expect("free");
    assert_eq!(first, store.logins_dir().join("main+1"));
    assert!(first.is_dir(), "made by the claim");
    assert_eq!(
        store.claim_staging_dir("main").expect("free"),
        store.logins_dir().join("main+2"),
        "the first claim's home is its own"
    );
    std::fs::create_dir_all(store.logins_dir().join("main+3")).expect("mkdir");
    assert_eq!(
        store.claim_staging_dir("main").expect("free"),
        store.logins_dir().join("main+4")
    );
    assert!(store.claim_staging_dir("Not A Label").is_err());
}

/// A sign-in task naming a step this build does not know, as a newer build sharing the file
/// writes, reads as no task and leaves the binding and every other row known (measured: 0.199.0
/// refused the whole file over a later build's `renew`). NEGATIVE CONTROL: a task with no step,
/// or a known step with no timestamp, is damage and still refuses the file.
#[test]
fn a_sign_in_task_this_build_does_not_know_is_dropped_and_the_bindings_stay_known() {
    let binding = |task: &str| {
        format!(
            r#"{{"schema":1,"labels":{{"main":{{"dir":"{}","was":[],"sign_in_task":{task}}},
                "work-2":{{"dir":null,"was":[]}}}}}}"#,
            abs("/s/current")
        )
    };
    let newer = binding(&format!(r#"{{"action":"a-later-step","at":{NOW}}}"#));
    let read = Logins::from_json(&newer, NOW).expect("a newer step leaves the file readable");
    assert_eq!(read.labels.len(), 2, "every row stays known");
    let main = &read.labels["main"];
    assert_eq!(main.sign_in_task, None, "the unknown step is dropped");
    assert_eq!(main.dir.as_deref(), Some(abs("/s/current").as_str()));

    let known = binding(&format!(r#"{{"action":"login","at":{NOW}}}"#));
    let read = Logins::from_json(&known, NOW).expect("a known step reads");
    assert_eq!(
        read.labels["main"].sign_in_task.map(|task| task.action),
        Some(SignInAction::Login),
        "CONTROL: a known step is kept"
    );

    for (case, task) in [
        ("no step", format!(r#"{{"at":{NOW}}}"#)),
        ("step not a word", format!(r#"{{"action":7,"at":{NOW}}}"#)),
        ("no timestamp", r#"{"action":"login"}"#.to_owned()),
        (
            "unknown step, no timestamp",
            r#"{"action":"a-later-step"}"#.to_owned(),
        ),
    ] {
        assert!(
            Logins::from_json(&binding(&task), NOW).is_err(),
            "NEGATIVE CONTROL: {case} is damage and refuses the file"
        );
    }
}

#[test]
fn uncertain_binding_bytes_refuse_mutation_instead_of_erasing_identities() {
    for (case, bytes) in [
        ("torn", b"{\"schema\":".as_slice()),
        ("non-utf8", b"\xff".as_slice()),
        ("labels-shape", br#"{"schema":1,"labels":null}"#.as_slice()),
        (
            "relative-home",
            br#"{"schema":1,"labels":{"main":{"dir":"relative"}}}"#.as_slice(),
        ),
    ] {
        let dir = tmp(case);
        let store = Store::at(dir.path());
        store.ensure().expect("store");
        std::fs::write(store.logins_file(), bytes).expect("uncertain binding");
        let edited = std::cell::Cell::new(false);
        let result = store.transact_logins(NOW, &mut None, |_, logins| {
            edited.set(true);
            note_served_login(logins, "main", NOW);
            Ok(Vec::new())
        });
        assert!(result.is_err(), "{case}: {result:?}");
        assert!(!edited.get(), "{case}: no edit without known bindings");
        assert_eq!(std::fs::read(store.logins_file()).unwrap(), bytes, "{case}");
        assert!(!store.state_file().exists(), "{case}: state untouched");
    }
}

#[cfg(unix)]
#[test]
fn only_an_absent_binding_file_means_legacy_identities() {
    let dir = tmp("binding-absence");
    let store = Store::at(dir.path());
    let original = roster();
    assert_eq!(store.bound(&original, NOW).unwrap(), original);
    store.ensure().expect("store");
    let file = store.logins_file();
    let target = store.dir.join("missing-target");
    std::os::unix::fs::symlink(&target, &file).expect("dangling binding link");
    assert!(store.bound(&original, NOW).is_err(), "not a known absence");
    let edited = std::cell::Cell::new(false);
    assert!(
        store
            .transact_logins(NOW, &mut None, |_, _| {
                edited.set(true);
                Ok(Vec::new())
            })
            .is_err()
    );
    assert!(!edited.get());
    assert_eq!(std::fs::read_link(&file).unwrap(), target);
    std::fs::remove_file(&file).unwrap();

    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
    // SAFETY: one NUL-terminated, owned scratch path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(
        store.bound(&original, NOW).is_err(),
        "a FIFO is not bindings"
    );
    std::fs::remove_file(&file).unwrap();

    std::fs::write(&target, Logins::default().to_json()).unwrap();
    std::os::unix::fs::symlink(&target, &file).unwrap();
    assert_eq!(store.bound(&original, NOW).unwrap(), original);
}

#[test]
fn the_forecast_learns_from_notices_and_ends_and_never_lengthens() {
    let r = roster();
    let main = r.row("main").expect("main").clone();
    let (state, mut logins) = renewed(NOW);
    let b = |l: &Logins| l.get("main").expect("bound").clone();
    assert_eq!(forecast(&b(&logins)), Some(NOW + 21 * DAY));
    let at = |l: &Logins, t: i64| login_state(&main, l, &state, t);
    assert_eq!(at(&logins, NOW + 16 * DAY - 1), LoginState::Idle);
    assert_eq!(
        at(&logins, NOW + 16 * DAY),
        LoginState::Due { by: NOW + 21 * DAY }
    );
    assert_eq!(forecast(&b(&logins)), Some(NOW + 21 * DAY));

    assert!(note_login_notice(&mut logins, "main", 2, NOW + 10 * DAY));
    assert_eq!(forecast(&b(&logins)), Some(NOW + 12 * DAY));
    assert_eq!(
        at(&logins, NOW + 10 * DAY),
        LoginState::Due { by: NOW + 12 * DAY }
    );
    assert_eq!(b(&logins).lifetime(), 12 * DAY);
    assert!(!note_login_notice(
        &mut logins,
        "main",
        3,
        NOW + 10 * DAY + HOUR
    ));
    assert_eq!(forecast(&b(&logins)), Some(NOW + 12 * DAY));
    let before = logins.clone();
    assert!(!note_login_notice(&mut logins, "main", 0, NOW + 11 * DAY));
    assert!(!note_login_notice(&mut logins, "main", 4, NOW + 11 * DAY));
    assert_eq!(logins, before);

    let mut state2 = state.clone();
    note_renewed(
        &mut state2,
        &mut logins,
        "main",
        None,
        "/s/main.2",
        None,
        NOW + 11 * DAY,
    );
    assert_eq!(forecast(&b(&logins)), Some(NOW + 23 * DAY));
    let events = note_login_end(&mut state2, &mut logins, "main", NOW + 20 * DAY);
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Auth, Kind::LoginEnded]
    );
    assert_eq!(b(&logins).lifetime(), 9 * DAY);
    assert!(
        note_login_end(&mut state2, &mut logins, "main", NOW + 20 * DAY + 60).is_empty(),
        "the same end, asked again, writes nothing"
    );
    note_renewed(
        &mut state2,
        &mut logins,
        "main",
        None,
        "/s/main.3",
        None,
        NOW + 21 * DAY,
    );
    note_login_end(&mut state2, &mut logins, "main", NOW + 22 * DAY);
    assert_eq!(
        b(&logins).lifetime(),
        9 * DAY,
        "a revocation is no lifetime"
    );

    let (_, mut short) = renewed(NOW);
    note_login_notice(&mut short, "main", 1, NOW + DAY);
    assert_eq!(b(&short).lifetime(), LIFETIME_MIN_S);
    let long = Binding {
        lifetime_s: 90 * DAY,
        ..Binding::default()
    };
    assert_eq!(long.lifetime(), LIFETIME_MAX_S);
    let mut fresh = Logins::default();
    assert_eq!(fresh.get("main").and_then(forecast), None);
    note_login_notice(&mut fresh, "main", 3, NOW);
    assert_eq!(fresh.get("main").and_then(forecast), Some(NOW + 3 * DAY));
}

#[test]
fn the_states_in_their_order() {
    let r = roster();
    let main = r.row("main").expect("main").clone();
    let (mut state, mut logins) = renewed(NOW);
    let at = |s: &State, l: &Logins, t: i64| login_state(&main, l, s, t);
    assert_eq!(at(&state, &logins, NOW), LoginState::Idle);
    note_served_login(&mut logins, "main", NOW);
    assert_eq!(at(&state, &logins, NOW + 6 * HOUR - 1), LoginState::Ok);
    assert_eq!(at(&state, &logins, NOW + 6 * HOUR), LoginState::Idle);
    note_login_notice(&mut logins, "main", 1, NOW);
    assert_eq!(at(&state, &logins, NOW), LoginState::Due { by: NOW + DAY });
    note_login_end(&mut state, &mut logins, "main", NOW + HOUR);
    assert_eq!(at(&state, &logins, NOW + HOUR), LoginState::Expired);
    for (s, word) in [
        (LoginState::Expired, "expired"),
        (LoginState::Due { by: 0 }, "due"),
        (LoginState::Ok, "ok"),
        (LoginState::Idle, "idle"),
    ] {
        assert_eq!(s.as_str(), word);
    }
}

#[test]
fn an_ended_renewable_login_is_due_and_never_signed_out() {
    let r = roster();
    let enrolled = every(&r);
    let (mut state, mut logins) = renewed(NOW);
    note_login_end(&mut state, &mut logins, "main", NOW + HOUR);
    assert_eq!(renew_due(&r, &logins, &state, &NONE, NOW + HOUR), ["main"]);
    assert!(signed_out(&r, &state, &enrolled, NOW + HOUR).is_empty());
    note_auth(&mut state, "work-2", Auth::SignedOut, NOW + HOUR);
    assert_eq!(signed_out(&r, &state, &enrolled, NOW + HOUR), ["work-2"]);
}

#[test]
fn a_login_signed_back_in_is_ok_again_and_due_no_more() {
    let r = roster();
    let enrolled = every(&r);
    let (mut state, mut logins) = renewed(NOW);
    assert!(note_login_notice(&mut logins, "main", 2, NOW + HOUR));
    note_login_end(&mut state, &mut logins, "main", NOW + 2 * HOUR);
    assert_eq!(
        renew_due(&r, &logins, &state, &NONE, NOW + 2 * HOUR),
        ["main"]
    );
    let back = NOW + 3 * HOUR;
    let events = note_signed_back(&mut state, &mut logins, "main", back);
    assert!(
        events.iter().any(|e| e.kind == Kind::Auth && e.wakes()),
        "{events:?}"
    );
    assert_eq!(state.account("main").auth, Auth::Ok);
    let binding = logins.get("main").expect("bound");
    assert_eq!(
        (binding.renewed_at, binding.notice_end, binding.served_at),
        (Some(back), None, Some(back))
    );
    assert!(renew_due(&r, &logins, &state, &NONE, back).is_empty());
    note_auth(&mut state, "work-2", Auth::SignedOut, back);
    assert_eq!(signed_out(&r, &state, &enrolled, back), ["work-2"]);
    assert!(!note_signed_back(&mut state, &mut logins, "work-2", back + 60).is_empty());
    assert!(signed_out(&r, &state, &enrolled, back + 60).is_empty());
    assert!(logins.get("work-2").is_none(), "no binding made");
    let before = (state.clone(), logins.clone());
    assert!(note_signed_back(&mut state, &mut logins, "main", back + HOUR).is_empty());
    assert_eq!((state.clone(), logins.clone()), before);
    note_auth(&mut state, "work-2", Auth::SignedOut, back + 2 * HOUR);
    assert!(note_signed_back(&mut state, &mut logins, "work-2", back + HOUR).is_empty());
    assert_eq!(state.account("work-2").auth, Auth::SignedOut);
}

#[test]
fn due_logins_and_none_working_are_the_renewable_rows() {
    let r = roster();
    let enrolled = every(&r);
    let (mut state, mut logins) = renewed(NOW);
    assert!(renew_due(&r, &logins, &state, &NONE, NOW).is_empty());
    assert!(!none_working_within(
        &r,
        &logins,
        &state,
        &enrolled,
        NOW,
        NONE_SOON_S
    ));
    note_login_notice(&mut logins, "main", 3, NOW);
    assert_eq!(renew_due(&r, &logins, &state, &NONE, NOW), ["main"]);
    assert!(!none_working_within(
        &r,
        &logins,
        &state,
        &enrolled,
        NOW,
        NONE_SOON_S
    ));
    assert!(none_working_within(
        &r,
        &logins,
        &state,
        &enrolled,
        NOW + 2 * DAY,
        NONE_SOON_S
    ));
    note_login_end(&mut state, &mut logins, "main", NOW + HOUR);
    assert_eq!(renew_due(&r, &logins, &state, &NONE, NOW + HOUR), ["main"]);
    assert!(none_working_within(
        &r,
        &logins,
        &state,
        &enrolled,
        NOW + HOUR,
        NONE_SOON_S
    ));
    let mut ended = state.clone();
    note_auth(&mut ended, "work-2", Auth::SignedOut, NOW);
    assert_eq!(renew_due(&r, &logins, &ended, &NONE, NOW + HOUR), ["main"]);
    let nobody: BTreeSet<String> = ["work-2".to_string()].into();
    let bare = Roster {
        rows: vec![Row {
            dir: Some("/nowhere".into()),
            ..r.rows[0].clone()
        }],
    };
    // An ended login a session here ran on is due though nothing of it is enrolled here (the
    // walled tab's successor whose directory went, `lab claude-rotation-proof` R4); one no
    // session here ran on is not.
    assert_eq!(
        renew_due(&bare, &logins, &state, &NONE, NOW + HOUR),
        ["main"]
    );
    let theirs = OtherMacs {
        named: BTreeMap::from([("main".to_owned(), "m22".to_owned())]),
        ..OtherMacs::NONE
    };
    assert!(
        renew_due(&bare, &logins, &state, &theirs, NOW + HOUR).is_empty(),
        "its source gives its account to another Mac"
    );
    let unread = OtherMacs {
        unread: BTreeSet::from(["main".to_owned()]),
        ..OtherMacs::NONE
    };
    assert!(
        renew_due(&bare, &logins, &state, &unread, NOW + HOUR).is_empty(),
        "this Mac cannot read whether its source gives its account to it"
    );
    let mut never_ran = state.clone();
    never_ran.accounts.get_mut("main").expect("main").ran_at = None;
    assert!(renew_due(&bare, &logins, &never_ran, &NONE, NOW + HOUR).is_empty());
    assert!(!none_working_within(
        &bare,
        &logins,
        &state,
        &nobody,
        NOW,
        NONE_SOON_S
    ));
    assert!(wrong_accounts(&r, &logins).is_empty());
    note_wrong_account(&mut logins, "main", NOW);
    assert_eq!(wrong_accounts(&r, &logins), ["main"]);
}

#[test]
fn a_bound_roster_reads_the_renewed_login_and_the_superseded_one_byte_for_byte() {
    let (main_1, work_2, w_1) = (abs("/s/main.1"), abs("/logins/work-2"), abs("/s/w.1"));
    let mut state = state();
    let mut logins = Logins::default();
    note_renewed(&mut state, &mut logins, "main", None, &main_1, None, NOW);
    note_renewed(
        &mut state,
        &mut logins,
        "work-2",
        Some(&work_2),
        &w_1,
        None,
        NOW,
    );
    let b = bound(&roster(), &logins);
    assert_eq!(b.rows[0].dir.as_deref(), Some(main_1.as_str()));
    assert_eq!(b.rows[1].dir.as_deref(), Some(w_1.as_str()));
    assert_eq!(
        b.rows[0].email,
        roster().rows[0].email,
        "nothing else of the row moves"
    );
    let who = |login: Option<&str>| ProcessLogin {
        login: login.map(str::to_owned),
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let on = |login: Option<&str>| {
        let me = who(login).me(&b);
        (me.account, me.left)
    };
    assert_eq!(on(Some(&main_1)), (Some("main"), None));
    assert_eq!(
        on(None),
        (None, Some("main")),
        "the default login, superseded"
    );
    assert_eq!(on(Some(&work_2)), (None, Some("work-2")));
    assert_eq!(
        on(Some(&format!("{work_2}/"))),
        (None, None),
        "another string, another login"
    );
    assert_eq!(on(Some(&w_1)), (Some("work-2"), None));
    assert_eq!(judged_label(&b, &who(None)), Some("main"));
    let file = roster();
    let me = who(None).me(&file);
    assert_eq!((me.account, me.left), (Some("main"), None));
    assert_eq!(who(Some(&main_1)).me(&file).label(), None);
    let dir = tmp("bound");
    let store = Store::at(dir.path());
    store.save_logins(&logins).expect("save");
    assert_eq!(store.bound(&roster(), NOW).expect("known identities"), b);
    std::fs::write(store.logins_file(), "{\"schema\":9}").expect("write");
    assert!(store.bound(&roster(), NOW).is_err(), "unknown identities");
}

#[test]
fn a_superseded_logins_usage_is_recorded_for_its_rows_account() {
    let (work_2, w_1) = (abs("/logins/work-2"), abs("/s/w.1"));
    let mut state = state();
    let mut logins = Logins::default();
    note_renewed(
        &mut state,
        &mut logins,
        "work-2",
        Some(&work_2),
        &w_1,
        None,
        NOW,
    );
    let b = bound(&roster(), &logins);
    let dir = tmp("usage");
    let store = Store::at(dir.path());
    store.save_logins(&logins).unwrap();
    let on = |login: &str| ProcessLogin {
        login: Some(login.to_owned()),
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let said = UsageSaid {
        at: NOW - 60,
        day: (NOW - 60).div_euclid(DAY),
        credits: Some(CreditsRead { until: None }),
        warning: Some(WarningRead {
            weekly: true,
            pct: 92,
            reset: None,
        }),
        week: None,
    };
    let old = on(&work_2);
    let me = old.me(&b);
    assert_eq!((me.account, me.left), (None, Some("work-2")), "superseded");
    assert_eq!(
        record_usage(&store, &b, "s-old", &old, &said, NOW),
        Recorded::Usage {
            account: "work-2".to_string(),
            events: 2,
            walled: None,
        }
    );
    let held = store.load(NOW).expect("ours").0;
    let work = &held.accounts["work-2"];
    assert!(
        matches!(work.credits, Some(Credits::InUse(_)))
            && work.warned_week.as_ref().is_some_and(|w| w.pct == Some(92)),
        "{work:?}"
    );
    let later = UsageSaid {
        at: NOW,
        warning: Some(WarningRead {
            weekly: true,
            pct: 95,
            reset: None,
        }),
        ..said
    };
    assert_eq!(
        record_usage(&store, &b, "s-new", &on(&w_1), &later, NOW).as_str(),
        "usage",
        "the renewed login's"
    );
    assert_eq!(
        record_usage(
            &store,
            &b,
            "s-x",
            &on(&abs("/logins/elsewhere")),
            &later,
            NOW
        ),
        Recorded::NotListed
    );
    assert_eq!(
        record_usage(&store, &roster(), "s-new", &on(&w_1), &later, NOW),
        Recorded::NotListed
    );
}

#[test]
fn the_one_accounts_login_follows_its_renewal() {
    const ONE: &str =
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"owner@example.com\"\n";
    let one = Roster::parse(ONE).expect("admits");
    assert_eq!(one.lone_login(), None, "the default login");
    let dir = tmp("lone");
    let store = Store::at(&dir.path().join("state"));
    let path = dir.path().join(ACCOUNTS_FILE);
    std::fs::write(&path, ONE).expect("roster");
    assert_eq!(lone_login_at(Some(&path), &store, NOW), None, "not renewed");
    let main_1 = abs("/s/main.1");
    let mut state = state();
    let mut logins = Logins::default();
    note_renewed(&mut state, &mut logins, "main", None, &main_1, None, NOW);
    store.save_logins(&logins).expect("save");
    assert_eq!(bound(&one, &logins).lone_login(), Some(main_1.as_str()));
    assert_eq!(
        lone_login_at(Some(&path), &store, NOW).as_deref(),
        Some(main_1.as_str())
    );
    std::fs::write(&path, roster_text()).expect("two rows");
    assert_eq!(bound(&roster(), &logins).lone_login(), None);
    assert_eq!(lone_login_at(Some(&path), &store, NOW), None, "two rows");
    std::fs::write(&path, "[[account]]\nlabel = 7\n").expect("refused");
    assert_eq!(lone_login_at(Some(&path), &store, NOW), None, "refused");
    assert_eq!(lone_login_at(None, &store, NOW), None, "no path");
}

#[test]
fn the_default_login_stays_the_own_rows_past_every_renewal() {
    let work_2 = abs("/logins/work-2");
    let mut state = state();
    let mut logins = Logins::default();
    for n in 1..=MAX_LEFT + 2 {
        let at = NOW + i64::try_from(n).expect("small") * DAY;
        note_renewed(
            &mut state,
            &mut logins,
            "main",
            None,
            &abs(&format!("/s/main.{n}")),
            None,
            at,
        );
        note_renewed(
            &mut state,
            &mut logins,
            "work-2",
            Some(&work_2),
            &abs(&format!("/s/w.{n}")),
            None,
            at,
        );
    }
    let b = bound(&roster(), &logins);
    let left = |login: Option<&str>| {
        ProcessLogin {
            login: login.map(str::to_owned),
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        }
        .me(&b)
        .left
    };
    assert_eq!(left(None), Some("main"), "the default login, still main's");
    assert_eq!(left(Some(&abs("/s/main.1"))), None, "past the bound");
    assert_eq!(left(Some(&work_2)), None);
    assert_eq!(
        left(Some(&abs(&format!("/s/w.{}", MAX_LEFT + 1)))),
        Some("work-2")
    );
}

#[test]
fn a_login_end_is_advised_the_sessions_own_label_and_never_another() {
    let r = Roster::parse(&format!(
        "{}\n[[account]]\nlabel = \"pool-a\"\nclass = \"shared\"\ndir = \"{}\"\n",
        roster_text(),
        abs("/logins/pool-a")
    ))
    .expect("admits");
    let enrolled = every(&r);
    let mut state = state();
    for label in ["main", "pool-a"] {
        note_auth(&mut state, label, Auth::Ok, NOW - DAY);
    }
    note_auth(&mut state, "main", Auth::SignedOut, NOW - HOUR);
    let on_main = Me {
        account: Some("main"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let advise_at = |roster: &Roster, state: &State, me: &Me<'_>, point: &Point, mode: Mode| {
        advise(roster, state, &enrolled, me, point, mode, NOW)
    };
    for mode in [Mode::Early, Mode::Wall, Mode::Off] {
        assert_eq!(
            advise_at(&r, &state, &on_main, &Point::LoginEnded, mode),
            Advice::Stay(StayWhy::NoCandidate),
            "{mode:?}: a usable account is never moved to"
        );
    }
    assert_eq!(
        advise_at(
            &r,
            &state,
            &on_main,
            &Point::LostLogin { seen: NOW },
            Mode::Early
        ),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false,
            yields: None,
        }
    );
    let mut logins = Logins::default();
    let mut renewed_state = state.clone();
    note_renewed(
        &mut renewed_state,
        &mut logins,
        "main",
        None,
        "/s/main.1",
        None,
        NOW,
    );
    let b = bound(&r, &logins);
    let left = ProcessLogin {
        login: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let me = left.me(&b);
    assert_eq!((me.account, me.left), (None, Some("main")));
    for mode in [Mode::Early, Mode::Off] {
        for (point, ahead) in [
            (Point::LoginEnded, false),
            (Point::LostLogin { seen: NOW }, false),
            (Point::TurnEnd, true),
        ] {
            assert_eq!(
                advise_at(&b, &renewed_state, &me, &point, mode),
                Advice::Move {
                    to: "main".into(),
                    ahead,
                    rotates: false,
                    takes_canary: false,
                    yields: None,
                },
                "{mode:?} {point:?}"
            );
        }
    }
    let mut dead = renewed_state.clone();
    note_auth(&mut dead, "main", Auth::SignedOut, NOW + 1);
    assert_eq!(
        advise_at(&b, &dead, &me, &Point::LoginEnded, Mode::Early),
        Advice::Stay(StayWhy::NoCandidate)
    );
    assert_eq!(
        advise_at(&b, &dead, &me, &Point::TurnEnd, Mode::Early),
        Advice::Stay(StayWhy::NothingDue)
    );
    let keyed = Me {
        route: crate::harness::accounts::AuthRoute::Outside,
        ..me
    };
    assert_eq!(
        advise_at(&b, &renewed_state, &keyed, &Point::TurnEnd, Mode::Off),
        Advice::Stay(StayWhy::NotAStoredLogin)
    );
    let wall = Point::Wall {
        window: "five_hour".into(),
        at: Some(NOW),
    };
    assert_eq!(
        advise_at(&b, &renewed_state, &me, &wall, Mode::Off),
        Advice::Stay(StayWhy::Off)
    );
    assert!(matches!(
        advise_at(&b, &renewed_state, &me, &wall, Mode::Early),
        Advice::Move { ref to, rotates: true, .. } if to == "work-2"
    ));
}

#[test]
fn a_renewal_made_under_a_wall_is_followed_at_once() {
    let r = roster();
    let enrolled = every(&r);
    let until = NOW + 5 * DAY;
    for window in ["seven_day_opus", "five_hour"] {
        let walled = |state: &mut State| {
            assert!(!note_wall(state, "main", window, Some(until), NOW - HOUR, NOW).is_empty());
        };
        let (mut state, logins) = renewed(NOW - 30 * 60);
        walled(&mut state);
        assert!(
            matches!(
                condition(&r.rows[0], &state, true, NOW),
                Condition::Limited { .. }
            ),
            "{window}: the account is limited"
        );
        let b = bound(&r, &logins);
        let on_old = ProcessLogin {
            login: None,
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        };
        let me = on_old.me(&b);
        assert_eq!((me.account, me.left), (None, Some("main")));
        let advise_at = |state: &State, me: &Me<'_>, point: &Point, mode: Mode| {
            advise(&b, state, &enrolled, me, point, mode, NOW)
        };
        for mode in [Mode::Off, Mode::Early] {
            for (point, ahead) in [
                (Point::LoginEnded, false),
                (Point::LostLogin { seen: NOW }, false),
                (Point::TurnEnd, true),
            ] {
                assert_eq!(
                    advise_at(&state, &me, &point, mode),
                    Advice::Move {
                        to: "main".into(),
                        ahead,
                        rotates: false,
                        takes_canary: false,
                        yields: None,
                    },
                    "{window} {mode:?} {point:?}: the renewed login followed"
                );
            }
        }
        let mut own = self::state();
        note_auth(&mut own, "main", Auth::SignedOut, NOW - HOUR);
        walled(&mut own);
        let on_own = Me {
            account: Some("main"),
            left: None,
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        };
        assert_eq!(
            advise(
                &r,
                &own,
                &enrolled,
                &on_own,
                &Point::LoginEnded,
                Mode::Off,
                NOW
            ),
            Advice::Stay(StayWhy::NoCandidate),
            "{window}"
        );
        note_auth(&mut state, "main", Auth::SignedOut, NOW);
        assert_eq!(
            advise_at(&state, &me, &Point::LoginEnded, Mode::Off),
            Advice::Stay(StayWhy::NoCandidate),
            "{window}"
        );
        assert_eq!(
            advise_at(&state, &me, &Point::TurnEnd, Mode::Off),
            Advice::Stay(StayWhy::NothingDue),
            "{window}"
        );
    }
}

#[test]
fn an_automatic_sign_in_is_written_once_per_finding_and_a_new_login_ends_its_failure() {
    let r = roster();
    let routes = "profile(none) mailbox(none configured) vault(not a member) \
                  headless-browser(none installed) hosts(none configured)";
    let mut logins = Logins::default();
    logins.labels.insert(
        "work-2".into(),
        Binding {
            dir: r.row("work-2").unwrap().dir.clone(),
            ..Binding::default()
        },
    );
    let events = admitted_observation(&mut logins, "work-2", NOW, |logins| {
        note_sign_in_attempt(logins, "work-2", AttemptOutcome::NoRoute, routes, NOW)
    });
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].kind, events[0].account.as_str()),
        (Kind::SignIn, "work-2")
    );
    assert_eq!(events[0].why, format!("{AUTO_SIGN_IN} no-route: {routes}"));
    let mut unadmitted = logins.clone();
    assert!(
        note_sign_in_attempt(
            &mut unadmitted,
            "work-2",
            AttemptOutcome::NoRoute,
            routes,
            NOW + HOUR
        )
        .is_empty()
    );
    assert!(unadmitted.get("work-2").unwrap().sign_in_attempt.is_some());
    assert!(
        current_sign_in_attempt(r.row("work-2").unwrap(), &unadmitted).is_none(),
        "an unadmitted duplicate retains history but cannot retain current authority"
    );
    assert!(
        admitted_observation(&mut logins, "work-2", NOW + HOUR, |logins| {
            note_sign_in_attempt(
                logins,
                "work-2",
                AttemptOutcome::NoRoute,
                routes,
                NOW + HOUR,
            )
        })
        .is_empty(),
        "the same finding is not written again"
    );
    assert_eq!(
        logins.get("work-2").and_then(|b| b.sign_in_attempt.clone()),
        Some(SignInAttempt {
            outcome: AttemptOutcome::NoRoute,
            routes: routes.to_owned(),
            since: NOW,
            tried_at: NOW,
        }),
        "since when it has been so"
    );
    let long = "mailbox(none configured) ".repeat(20);
    assert_eq!(
        admitted_observation(&mut logins, "main", NOW, |logins| {
            note_sign_in_attempt(logins, "main", AttemptOutcome::Failed, &long, NOW)
        })
        .len(),
        1
    );
    let bounded = &logins
        .get("main")
        .and_then(|b| b.sign_in_attempt.clone())
        .unwrap();
    assert!(bounded.routes.len() <= ROUTES_MAX);

    let text = logins.to_json();
    assert!(text.contains("\"sign_in_attempt\":"), "{text}");
    assert_eq!(Logins::from_json(&text, NOW), Ok(logins.clone()));
    let unread = Logins::from_json(
        &text.replace("\"outcome\":\"no-route\"", "\"outcome\":\"someday\""),
        NOW,
    )
    .expect("an attempt it cannot read is dropped, never the whole file");
    assert_eq!(unread.get("work-2").unwrap().sign_in_attempt, None);
    assert_eq!(
        unread.get("work-2").unwrap().dir,
        logins.get("work-2").unwrap().dir
    );

    let mut signed_out = state();
    note_auth(&mut signed_out, "main", Auth::SignedOut, NOW - HOUR);
    assert_eq!(
        failing_sign_ins(&r, &r, &signed_out, &logins, NOW)
            .into_iter()
            .map(|(label, attempt)| (label, attempt.outcome))
            .collect::<Vec<_>>(),
        [
            ("main".to_owned(), AttemptOutcome::Failed),
            ("work-2".to_owned(), AttemptOutcome::NoRoute)
        ],
        "a signed-out row and a row never enrolled, in roster order"
    );
    assert_eq!(
        failing_sign_ins(&r, &r, &state(), &logins, NOW)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>(),
        ["work-2"],
        "a row whose login works says nothing of an old failure"
    );
    let mut due = logins.clone();
    due.labels
        .get_mut("main")
        .expect("main's binding")
        .notice_end = Some(NOW + DAY);
    assert_eq!(
        failing_sign_ins(&r, &r, &state(), &due, NOW)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>(),
        ["main", "work-2"],
        "a renewal of a login due for one that failed is still news"
    );

    let mut state = signed_out;
    note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        &abs("/s/main.1"),
        None,
        NOW + HOUR,
    );
    assert_eq!(logins.get("main").unwrap().sign_in_attempt, None);
    assert_eq!(
        admitted_observation(&mut logins, "main", NOW + HOUR, |logins| {
            note_sign_in_attempt(
                logins,
                "main",
                AttemptOutcome::SignedIn,
                "mailbox",
                NOW + HOUR,
            )
        })
        .len(),
        1
    );
    note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        &abs("/s/main.2"),
        None,
        NOW + 2 * HOUR,
    );
    assert_eq!(
        logins
            .get("main")
            .and_then(|b| b.sign_in_attempt.as_ref())
            .map(|a| a.outcome),
        Some(AttemptOutcome::SignedIn),
        "the route that made the login stays named"
    );
    assert_eq!(
        failing_sign_ins(&r, &r, &state, &logins, NOW + 2 * HOUR)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>(),
        ["work-2"]
    );
}

/// A HAND RUN THAT FAILED FOR ANOTHER REASON NEVER LIFTS A PAGE'S HOLD: the page that stopped
/// the row stays its current attempt, with the time its wait counts from, and the hand run is
/// one ledger row. CONTROLS: with no page on record the hand failure is the row's last
/// sign-in, and a hand run that signed the row in replaces the page's record.
#[test]
fn a_failed_hand_run_leaves_the_page_that_stopped_its_row_on_record() {
    let page = "claude.ai bot-check";
    let by_hand = "by hand: claude auth login exited 1 with no login made";
    let stopped = SignInAttempt {
        outcome: AttemptOutcome::Blocked,
        routes: page.to_owned(),
        since: NOW,
        tried_at: NOW,
    };
    let attempt = |logins: &Logins| logins.get("main").and_then(|b| b.sign_in_attempt.clone());
    let hand = |logins: &mut Logins, outcome: AttemptOutcome| {
        admitted_observation(logins, "main", NOW + HOUR, |logins| {
            note_hand_sign_in_attempt(logins, "main", outcome, by_hand, NOW + HOUR)
        })
    };

    let mut logins = Logins::default();
    admitted_observation(&mut logins, "main", NOW, |logins| {
        note_sign_in_attempt(logins, "main", AttemptOutcome::Blocked, page, NOW)
    });
    assert_eq!(attempt(&logins), Some(stopped.clone()));
    let events = hand(&mut logins, AttemptOutcome::Failed);
    assert_eq!(events.len(), 1, "the hand run is on the ledger");
    assert_eq!(events[0].why, format!("{HAND_SIGN_IN} failed: {by_hand}"));
    assert_eq!(
        attempt(&logins),
        Some(stopped),
        "the page's record, and the time its wait counts from, stand"
    );

    let events = hand(&mut logins, AttemptOutcome::SignedIn);
    assert_eq!(events.len(), 1);
    assert_eq!(
        attempt(&logins).map(|a| (a.outcome, a.since)),
        Some((AttemptOutcome::SignedIn, NOW + HOUR)),
        "CONTROL: a hand run that signed the row in replaces the page's record"
    );

    let mut plain = Logins::default();
    admitted_observation(&mut plain, "main", NOW, |logins| {
        note_sign_in_attempt(logins, "main", AttemptOutcome::NoRoute, "none", NOW)
    });
    assert_eq!(hand(&mut plain, AttemptOutcome::Failed).len(), 1);
    assert_eq!(
        attempt(&plain),
        Some(SignInAttempt {
            outcome: AttemptOutcome::Failed,
            routes: by_hand.to_owned(),
            since: NOW + HOUR,
            tried_at: NOW + HOUR,
        }),
        "CONTROL: with no page on record the hand failure is the row's last sign-in"
    );
}

/// A LOGIN SIGNED BACK IN WHERE IT STANDS ENDS ITS ROW'S FAILED AUTOMATIC SIGN-IN, as a renewal
/// does: once that login is due, the old failure is no news of it, and a failure of its own
/// renewal is written afresh, since its own time. CONTROL: while the row is still signed out the
/// failure stands.
#[test]
fn a_login_signed_back_in_ends_its_rows_old_sign_in_failure() {
    let r = roster();
    let routes = "profile(none) mailbox(none configured)";
    let mut logins = Logins::default();
    let mut state = state();
    note_auth(&mut state, "main", Auth::SignedOut, NOW - HOUR);
    admitted_observation(&mut logins, "main", NOW, |logins| {
        note_sign_in_attempt(logins, "main", AttemptOutcome::NoRoute, routes, NOW)
    });
    let failing = |state: &State, logins: &Logins, at: i64| {
        failing_sign_ins(&r, &r, state, logins, at)
            .into_iter()
            .filter(|(label, _)| label == "main")
            .map(|(_, attempt)| attempt.since)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        failing(&state, &logins, NOW),
        [NOW],
        "the control: signed out"
    );
    assert!(!note_signed_back(&mut state, &mut logins, "main", NOW + HOUR).is_empty());
    assert_eq!(logins.get("main").unwrap().sign_in_attempt, None);
    let at = NOW + 2 * HOUR;
    logins
        .labels
        .get_mut("main")
        .expect("main's binding")
        .notice_end = Some(at + DAY);
    assert!(
        needs_sign_in(r.row("main").unwrap(), &state, &logins, true, at),
        "the login is due"
    );
    assert!(
        failing(&state, &logins, at).is_empty(),
        "no failure from before the login is told of it"
    );
    admitted_observation(&mut logins, "main", at, |logins| {
        note_sign_in_attempt(logins, "main", AttemptOutcome::NoRoute, routes, at)
    });
    assert_eq!(
        failing(&state, &logins, at),
        [at],
        "its own renewal's failure, since its own time"
    );
}

#[test]
fn claude_codes_own_warning_is_read_at_any_percent_until_its_window_ends() {
    let mut account = Account::default();
    assert!(!account.warned(NOW));
    account.warned_week = Some(WindowSaid {
        pct: Some(80),
        reset: Some(NOW + DAY),
        at: NOW - HOUR,
    });
    assert!(account.warned(NOW), "a weekly warning at 80%");
    assert!(!account.low(NOW), "well before the account is low");
    assert!(!account.warned(NOW + DAY), "its window ended");
    account.warned_week = None;
    account.warned_session = Some(WindowSaid {
        pct: Some(90),
        reset: None,
        at: NOW - HOUR,
    });
    assert!(account.warned(NOW), "a five-hour warning");
    assert!(
        !account.warned(NOW + 4 * HOUR),
        "five hours after it was read"
    );
}

/// RULE 4's USE CONDITION (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15): a due login is renewed
/// here only while a session here ran on it within a week and no other Mac holds its senior
/// claim. NEGATIVE CONTROL: without the use condition the idle login is due again.
#[test]
fn a_due_login_is_renewed_only_while_this_mac_uses_it() {
    let r = roster();
    let (mut state, mut logins) = renewed(NOW);
    note_login_end(&mut state, &mut logins, "main", NOW + HOUR);
    let due = |state: &State, at: i64| renew_due(&r, &logins, state, &NONE, at);
    assert_eq!(due(&state, NOW + HOUR), ["main"]);
    assert_eq!(due(&state, NOW + USED_WITHIN_S - 1), ["main"]);
    assert!(
        due(&state, NOW + USED_WITHIN_S).is_empty(),
        "a week with no session here"
    );
    let mut never = state.clone();
    never.accounts.get_mut("main").unwrap().ran_at = None;
    assert!(due(&never, NOW + HOUR).is_empty(), "never run on here");
    let mut theirs = state.clone();
    theirs.machine = "this-mac".into();
    theirs.leases.insert(
        "main".into(),
        AccountLease {
            holder: "claim:other-mac".into(),
            until: NOW + 2 * HOUR,
            at: NOW,
        },
    );
    assert!(
        due(&theirs, NOW + HOUR).is_empty(),
        "another Mac holds its senior claim"
    );
    let mut ours = theirs.clone();
    ours.leases.get_mut("main").unwrap().holder = "this-mac".into();
    assert_eq!(due(&ours, NOW + HOUR), ["main"], "this Mac's own claim");
    assert_eq!(
        due(&theirs, NOW + 3 * HOUR),
        ["main"],
        "a lapsed claim is no claim"
    );
    assert_eq!(
        without_use_condition(|| due(&never, NOW + HOUR)),
        ["main"],
        "CONTROL: the timer renewal the use condition stops"
    );
}
