// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn schema_and_correlation_are_exact_and_do_not_admit_extra_authority() {
    const ID: &str = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        request("snapshot schema=1 request=0123456789abcdef0123456789abcdef"),
        Some((1, ID))
    );
    assert_eq!(
        request("snapshot schema=2 request=0123456789abcdef0123456789abcdef"),
        Some((2, ID)),
        "the schema a client asks is the one it is answered"
    );
    for bad in [
        "snapshot schema=3 request=0123456789abcdef0123456789abcdef",
        "snapshot schema=0 request=0123456789abcdef0123456789abcdef",
        "snapshot schema=21 request=0123456789abcdef0123456789abcdef",
        "snapshot schema= request=0123456789abcdef0123456789abcdef",
        "snapshot request=0123456789abcdef0123456789abcdef",
        "snapshot schema=2 request=0123456789ABCDEF0123456789abcdef",
        "snapshot schema=2 request=0123456789abcdef0123456789abcdef target=other",
        "snapshot schema=1 request=0123456789ABCDEF0123456789abcdef",
        "snapshot schema=1 request=short",
        "snapshot schema=1 request=0123456789abcdef0123456789abcdef target=other",
    ] {
        assert!(request(bad).is_none());
    }
}

#[test]
fn display_metadata_does_not_publish_addresses_or_credential_routes() {
    assert_eq!(
        masked_email(Some("person@example.com")).as_deref(),
        Some("p***@example.com")
    );
    assert_eq!(masked_email(Some("person\n@example.com")), None);
    assert_eq!(account_id("work"), account_id("work"));
    assert_ne!(account_id("work"), account_id("home"));
}

#[test]
fn observation_needs_no_resume_record_and_default_route_proves_no_assignment() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(root.path().join("home"));
    env.state = root.path().join("state");
    env.config = Some(root.path().join("config/aterm.toml"));
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/accounts-codex.toml"),
        "[[account]]\nlabel='main'\nclass='dedicated'\n",
    )
    .unwrap();
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "captured-kernel-birth".into(),
    };
    let value = snapshot_in("bound-session", &proof, &env, 1, None)
        .unwrap()
        .value;
    assert_eq!(value["assignment"]["account_id"], Value::Null);
    assert_eq!(value["assignment"]["mode"].as_str(), Some("unknown"));
    assert_eq!(value["accounts"][0]["eligible"].as_bool(), Some(false));
    assert_eq!(
        value["session"]["process_birth"].as_str(),
        Some("captured-kernel-birth")
    );
    assert!(
        !root.path().join("home").exists(),
        "no resume or credential home was read or created"
    );
    assert!(!env.state.exists(), "observation creates no account state");
    let mut later = env.clone();
    later.now += 10;
    let again = snapshot_in("bound-session", &proof, &later, 1, None)
        .unwrap()
        .value;
    assert_eq!(
        value["revision"], again["revision"],
        "clock ticks do not stale authority metadata"
    );
    assert_ne!(value["observed_at"], again["observed_at"]);
}

#[test]
fn missing_or_replaced_caller_never_receives_a_snapshot() {
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "birth-a".into(),
    };
    let replaced = crate::self_claim::Proven {
        pid: 42,
        start: "birth-b".into(),
    };
    assert!(same_binding(&proof, Some(&proof), true));
    assert!(!same_binding(&proof, None, true));
    assert!(!same_binding(&proof, Some(&replaced), true));
    assert!(!same_binding(&proof, Some(&proof), false));
}

#[test]
fn live_workers_keep_their_bounded_slot_after_request_expiry() {
    let mut table = Jobs::new();
    for i in 0..32 {
        table.insert(
            ("sid".into(), format!("{i:032x}"), 1),
            Arc::new(Job {
                proof: crate::self_claim::Proven {
                    pid: 42,
                    start: "birth".into(),
                },
                started: Instant::now() - Duration::from_secs(121),
                result: Mutex::new(None),
            }),
        );
    }
    prune(&mut table);
    assert_eq!(table.len(), 32);
    let key: (String, String, u8) = ("sid".into(), format!("{:032x}", 0), 1);
    assert!(
        !table.contains_key(&(key.0.clone(), key.1.clone(), 2)),
        "a schema 2 request of the same id is another worker's"
    );
    let first = Arc::clone(table.get(&key).unwrap());
    assert!(
        Arc::ptr_eq(&first, table.get(&key).unwrap()),
        "same request polls one worker"
    );
    *first.result.lock().unwrap() = Some(None);
    prune(&mut table);
    assert_eq!(
        table.len(),
        31,
        "only a terminal expired worker releases its slot"
    );
}

/// A pool of this machine's own login (`main`, not signed in here) and a stored login `work`
/// under `root`, as the snapshot's environment.
fn pool_env(root: &std::path::Path) -> (Env, std::path::PathBuf) {
    let mut env = Env::from_process().unwrap();
    env.home = Some(root.join("home"));
    env.state = root.join("state");
    env.config = Some(root.join("config/aterm.toml"));
    env.now = 1_792_700_000;
    let work = root.join("logins/codex/work");
    for dir in [&root.join("config"), &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(work.join("auth.json"), "{}").unwrap();
    std::fs::write(
        root.join("config/accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"person@example.com\"\n\
             dir = {}\n",
            crate::test_host::toml(&work)
        ),
    )
    .unwrap();
    (env, work)
}

/// A snapshot's text with the two fields that differ from run to run (the pool's revision and
/// this host's epoch) replaced by their names.
fn text_of(mut value: Value) -> String {
    let top = value.as_object_mut().unwrap();
    top.insert("revision".into(), Value::from("REVISION"));
    let session = top
        .get_mut("session")
        .and_then(Value::as_object_mut)
        .unwrap();
    session.insert("epoch".into(), Value::from("EPOCH"));
    aterm_json::to_string(&value).unwrap()
}

/// The line aterm validated for a codex-alab that runs `program` on the login `auth` names
/// (`None`: this machine's own).
fn launched(program: &std::path::Path, auth: Option<&std::path::Path>) -> self_claimed::Resume {
    let program = program.display().to_string();
    self_claimed::Resume {
        argv: vec![
            program,
            "resume".into(),
            "0badf00d-1111-2222-3333-444455556666".into(),
        ],
        session: "0badf00d-1111-2222-3333-444455556666".into(),
        codex_home: None,
        codex_auth_path: auth.map(|auth| auth.display().to_string()),
        cwd: None,
    }
}

/// `resume`'s launch, its exec environment setting no credential variable.
fn running(resume: &self_claimed::Resume) -> crate::harness_host::RunningLaunch {
    crate::harness_host::RunningLaunch {
        resume: resume.clone(),
        ahead: Vec::new(),
    }
}

fn proof() -> crate::self_claim::Proven {
    crate::self_claim::Proven {
        pid: 42,
        start: "birth".into(),
    }
}

/// THE SCHEMA 1 SNAPSHOT IS BYTE FOR BYTE WHAT IT WAS: a client that asks schema 1 is told no
/// assignment, no eligible row and no capability, whatever a move could ask. The text below is
/// what the snapshot answered for this pool before schema 2 existed.
#[test]
fn a_schema_1_snapshot_is_what_it_was() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (env, work) = pool_env(root.path());
    let program = aterm_agent::harness::codex_alab::lay_package(&root.path().join("store"), true);
    let unasked = snapshot_in("bound-session", &proof(), &env, 1, None).unwrap();
    assert_eq!(text_of(unasked.value), SCHEMA_1);
    // A session a schema 2 snapshot would offer a move to is told nothing more under schema 1.
    let on_work = launched(&program, Some(&work.join("auth.json")));
    let movable =
        snapshot_in("bound-session", &proof(), &env, 1, Some(&running(&on_work))).unwrap();
    assert_eq!(text_of(movable.value.clone()), SCHEMA_1);
    assert!(!movable.select && !movable.auto);
    let mut granted = movable;
    grant(&mut granted, true);
    assert_eq!(
        text_of(granted.value),
        SCHEMA_1,
        "no session's standing grants schema 1 a capability"
    );
}

const SCHEMA_1: &str = "{\"accounts\":[{\"condition\":\"not-enrolled\",\"eligible\":false,\"enrolled\":false,\"held\":false,\"id\":\"4e262b3b4b5afd6f1aad178108c7a27f56c1f3684c89d40695bbbf2752aac4cd\",\"label\":\"main\",\"masked_email\":null,\"quota\":{\"five_hour_reset_at\":null,\"five_hour_used_percent\":null,\"observed_at\":null,\"stale\":true,\"weekly_reset_at\":null,\"weekly_used_percent\":null}},{\"condition\":\"unseen\",\"eligible\":false,\"enrolled\":true,\"held\":false,\"id\":\"52f9c7fe9a2c1f06a11b8fa847d0036264d06e08b18fbf2808f9187c14dce3b1\",\"label\":\"work\",\"masked_email\":\"p***@example.com\",\"quota\":{\"five_hour_reset_at\":null,\"five_hour_used_percent\":null,\"observed_at\":null,\"stale\":true,\"weekly_reset_at\":null,\"weekly_used_percent\":null}}],\"assignment\":{\"account_id\":null,\"mode\":\"unknown\"},\"capabilities\":{\"add\":false,\"auto\":false,\"select\":false},\"observed_at\":1792700000,\"revision\":\"REVISION\",\"schema\":1,\"session\":{\"epoch\":\"EPOCH\",\"process_birth\":\"birth\",\"process_pid\":42,\"sid\":\"bound-session\"}}";

/// What a schema 2 snapshot says of a move: the row the session runs on, the pool's choice,
/// its pin and rotation, each row's `eligible`, and the pool's side of `select` and `auto`.
#[derive(Debug, PartialEq, Eq)]
struct Said {
    on: Option<String>,
    choice: Option<String>,
    pinned: bool,
    rotation: String,
    eligible: Vec<(String, bool)>,
    select: bool,
    auto: bool,
}

fn said(env: &Env, launch: Option<&self_claimed::Resume>) -> Said {
    said_of(env, launch.map(running).as_ref())
}

fn said_of(env: &Env, launch: Option<&crate::harness_host::RunningLaunch>) -> Said {
    let snapshot = snapshot_in("bound-session", &proof(), env, 2, launch).unwrap();
    let value = &snapshot.value;
    assert_eq!(value["schema"].as_u64(), Some(2));
    assert!(
        value["assignment"]
            .as_object()
            .is_some_and(|assignment| assignment.len() == 1),
        "schema 2's assignment names a row and no mode: {value:?}"
    );
    assert_eq!(
        value["capabilities"],
        json!({"add": false, "select": false, "auto": false}),
        "the worker grants nothing: the session's side is judged at reply time"
    );
    let rows = value["accounts"].as_array().unwrap();
    let label_of = |id: &Value| {
        id.as_str().map(|id| {
            let row = rows.iter().find(|row| row["id"].as_str() == Some(id));
            row.expect("a named row is a listed one")["label"]
                .as_str()
                .unwrap()
                .to_owned()
        })
    };
    Said {
        on: label_of(&value["assignment"]["account_id"]),
        choice: label_of(&value["pool"]["choice_id"]),
        pinned: value["pool"]["pinned"].as_bool().unwrap(),
        rotation: value["pool"]["rotation"].as_str().unwrap().to_owned(),
        eligible: rows
            .iter()
            .map(|row| {
                (
                    row["label"].as_str().unwrap().to_owned(),
                    row["eligible"].as_bool().unwrap(),
                )
            })
            .collect(),
        select: snapshot.select,
        auto: snapshot.auto,
    }
}

/// A SCHEMA 2 SNAPSHOT SAYS WHAT A MOVE MAY ASK. The row the session runs on comes from the
/// login its launch carried, as aterm validated it: a session on this machine's own login runs
/// on `main` and may ask for `work` (signed in here, never measured) and for the pool's choice;
/// one on `work` may ask for neither `work` nor the pool's choice, which is its own row. A wall
/// on record, rotation off, a login kept in the keyring, a program that keeps no row's login
/// its own, and a session aterm holds no validated launch of each take the move away, and none
/// of them names a row the snapshot does not list.
#[test]
fn a_schema_2_snapshot_says_what_a_move_may_ask() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (env, work) = pool_env(root.path());
    let home = env.home.clone().unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
    let program = aterm_agent::harness::codex_alab::lay_package(&root.path().join("store"), true);
    let rows = |main: bool, work: bool| vec![("main".to_owned(), main), ("work".to_owned(), work)];
    let on_main = launched(&program, None);
    let on_work = launched(&program, Some(&work.join("auth.json")));
    let first = said(&env, Some(&on_main));
    assert_eq!(first.on.as_deref(), Some("main"));
    assert_eq!(first.eligible, rows(false, true));
    assert_eq!((first.pinned, first.rotation.as_str()), (false, "early"));
    assert!(first.select, "the pool moves this session");
    let choice = first
        .choice
        .clone()
        .expect("a pool with two rows has a choice");
    assert_eq!(
        first.auto,
        choice != "main",
        "the pool's choice is offered only where it is another row: {first:?}"
    );
    let moved = said(&env, Some(&on_work));
    assert_eq!(moved.on.as_deref(), Some("work"));
    assert_eq!(moved.eligible, rows(true, false), "its own row is no move");
    assert_eq!(moved.choice.as_deref(), Some(choice.as_str()));
    assert_eq!(moved.auto, choice != "work", "{moved:?}");
    assert!(
        first.auto != moved.auto,
        "the pool's choice is one of the two rows, and is offered to the session on the other"
    );
    // A process whose exec environment set a credential Codex reads ahead of its login file runs
    // on that credential, whatever login its launch named: on no row.
    let ahead = aterm_agent::harness::relaunch::ahead_env(accounts::Cli::Codex);
    assert!(!ahead.is_empty(), "CONTROL: Codex reads such a credential");
    let on_token = said_of(
        &env,
        Some(&crate::harness_host::RunningLaunch {
            resume: on_main.clone(),
            ahead: vec![ahead[0]],
        }),
    );
    assert_eq!(
        (on_token.on, on_token.select, on_token.auto),
        (None, false, false),
        "a session on a credential variable runs on no row the pool may move it off"
    );

    let seat = accounts_pool::Seat {
        config_dir: env.config.as_deref().unwrap().parent().unwrap(),
        state: &env.state,
        home: &home,
        now: env.now,
    };
    seat.store(accounts::Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, env.now, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "work",
                "five_hour",
                Some(env.now + 3600),
                env.now - 60,
                env.now,
            ))
        })
        .unwrap();
    let walled = said(&env, Some(&on_main));
    assert_eq!(walled.eligible, rows(false, false), "a wall on record");
    assert!(
        walled.select,
        "the session can still be moved, to no row now"
    );

    let unclaimed = said(&env, None);
    assert_eq!(
        (
            unclaimed.on,
            unclaimed.choice,
            unclaimed.select,
            unclaimed.auto
        ),
        (None, None, false, false),
        "no validated launch: no row is the session's and nothing is offered"
    );
    assert_eq!(unclaimed.eligible, rows(false, false));

    let old = aterm_agent::harness::codex_alab::lay_package(&root.path().join("old"), false);
    let on_old = said(&env, Some(&launched(&old, None)));
    assert_eq!(on_old.on.as_deref(), Some("main"));
    assert!(
        !on_old.select && !on_old.auto,
        "a program that keeps no row's login its own is moved nowhere"
    );

    std::fs::write(
        env.config.as_deref().unwrap(),
        "[harness]\naccounts = 'off'\n",
    )
    .unwrap();
    let off = said(&env, Some(&on_work));
    assert_eq!(off.rotation, "off");
    assert_eq!(off.on.as_deref(), Some("work"), "it still runs on its row");
    assert!(!off.select && !off.auto, "rotation off moves no session");
    assert_eq!(off.eligible, rows(false, false));
    std::fs::remove_file(env.config.as_deref().unwrap()).unwrap();

    std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = 'keyring'\n",
    )
    .unwrap();
    let keyring = said(&env, Some(&on_main));
    assert_eq!(
        (keyring.on, keyring.select, keyring.auto),
        (None, false, false),
        "a login kept in the keyring is no row's"
    );
    assert_eq!(keyring.eligible, rows(false, false));
}

/// THE POOL AND THE SESSION BOTH HAVE TO ALLOW A MOVE ([`grant`]): `select` and `auto` are true
/// in the reply only where the pool offers them and the restart would be carried now, and
/// `add` is false whatever either says.
#[test]
fn a_capability_is_granted_only_where_the_pool_and_the_session_both_allow_it() {
    let capabilities = |select: bool, auto: bool, offered: bool| {
        let mut snapshot = Snapshot {
            value: json!({"capabilities": {"add": false, "select": false, "auto": false}}),
            select,
            auto,
        };
        grant(&mut snapshot, offered);
        let said = &snapshot.value["capabilities"];
        (
            said["add"].as_bool().unwrap(),
            said["select"].as_bool().unwrap(),
            said["auto"].as_bool().unwrap(),
        )
    };
    assert_eq!(capabilities(true, true, true), (false, true, true));
    assert_eq!(capabilities(true, false, true), (false, true, false));
    assert_eq!(capabilities(true, true, false), (false, false, false));
    assert_eq!(capabilities(false, false, true), (false, false, false));
}

/// A REPLY CARRIES ITS REQUEST, AND ONLY SCHEMA 2 IS GRANTED A CAPABILITY ([`reply`]): schema 2
/// asks whether the move would be carried and says `select` and `auto` by it; schema 1 does not
/// ask and says none, whatever its pool would offer. The schema 2 text below is the whole reply
/// for a session on this machine's own login, as a client parses it.
#[test]
fn a_reply_carries_its_request_and_only_schema_2_is_granted() {
    const ID: &str = "0123456789abcdef0123456789abcdef";
    let root = aterm_tempfile::tempdir().unwrap();
    let (env, _) = pool_env(root.path());
    let home = env.home.clone().unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
    let program = aterm_agent::harness::codex_alab::lay_package(&root.path().join("store"), true);
    let on_main = launched(&program, None);
    let body = |schema: u8, offered: Option<bool>| {
        let snapshot = snapshot_in(
            "bound-session",
            &proof(),
            &env,
            schema,
            Some(&running(&on_main)),
        )
        .expect("a snapshot");
        let said = reply(ID, schema, snapshot, || {
            offered.expect("schema 1 asks nothing of the session")
        });
        let body = said
            .strip_prefix("OK codex-accounts ")
            .and_then(|rest| rest.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("{said}"));
        aterm_json::from_str::<Value>(body).unwrap()
    };
    let granted = body(2, Some(true));
    assert_eq!(granted["request"].as_str(), Some(ID));
    assert_eq!(text_of(granted), SCHEMA_2);
    let held = body(2, Some(false));
    assert_eq!(
        held["capabilities"],
        json!({"add": false, "select": false, "auto": false}),
        "a restart that would not be carried offers no move"
    );
    let old = body(1, None);
    assert_eq!(old["request"].as_str(), Some(ID));
    assert_eq!(
        old["capabilities"],
        json!({"add": false, "select": false, "auto": false})
    );
    let forced = reply(
        ID,
        1,
        Snapshot {
            value: json!({"capabilities": {"add": false, "select": false, "auto": false}}),
            select: true,
            auto: true,
        },
        || true,
    );
    assert!(
        forced.contains("\"auto\":false") && forced.contains("\"select\":false"),
        "{forced}"
    );
}

const SCHEMA_2: &str = "{\"accounts\":[{\"condition\":\"unseen\",\"eligible\":false,\"enrolled\":true,\"held\":false,\"id\":\"4e262b3b4b5afd6f1aad178108c7a27f56c1f3684c89d40695bbbf2752aac4cd\",\"label\":\"main\",\"masked_email\":null,\"quota\":{\"five_hour_reset_at\":null,\"five_hour_used_percent\":null,\"observed_at\":null,\"stale\":true,\"weekly_reset_at\":null,\"weekly_used_percent\":null}},{\"condition\":\"unseen\",\"eligible\":true,\"enrolled\":true,\"held\":false,\"id\":\"52f9c7fe9a2c1f06a11b8fa847d0036264d06e08b18fbf2808f9187c14dce3b1\",\"label\":\"work\",\"masked_email\":\"p***@example.com\",\"quota\":{\"five_hour_reset_at\":null,\"five_hour_used_percent\":null,\"observed_at\":null,\"stale\":true,\"weekly_reset_at\":null,\"weekly_used_percent\":null}}],\"assignment\":{\"account_id\":\"4e262b3b4b5afd6f1aad178108c7a27f56c1f3684c89d40695bbbf2752aac4cd\"},\"capabilities\":{\"add\":false,\"auto\":false,\"select\":true},\"observed_at\":1792700000,\"pool\":{\"choice_id\":\"4e262b3b4b5afd6f1aad178108c7a27f56c1f3684c89d40695bbbf2752aac4cd\",\"pinned\":false,\"rotation\":\"early\"},\"request\":\"0123456789abcdef0123456789abcdef\",\"revision\":\"REVISION\",\"schema\":2,\"session\":{\"epoch\":\"EPOCH\",\"process_birth\":\"birth\",\"process_pid\":42,\"sid\":\"bound-session\"}}";

/// THE SESSION'S SIDE OF A MOVE IS THE RESTART'S OWN FENCES ([`move_offered`]): offered only
/// for the process the host's snapshot names, under the host's claim, with a shell under it to
/// type its relaunch, and under no other driver's lease.
#[test]
fn a_move_is_offered_only_where_the_restart_would_be_carried() {
    let ctx = crate::session_store::test_handle_on(7790, -1).ctx;
    let sid = ctx.self_id.as_str().to_string();
    let proof = crate::self_claim::Proven {
        pid: 0x3fff_ff31,
        start: "t".into(),
    };
    let publish = |pid: u32, shell: u32| {
        let copy = self_claimed::Copy {
            lifetime: None,
            pid,
            start: "t".into(),
            home: std::path::PathBuf::from("/u/.codex"),
            seen: self_claimed::Seen::Removed,
            resume: Ok(launched(std::path::Path::new("/opt/codex/bin/codex"), None)),
        };
        let resume = copy.resume.clone().unwrap();
        let snap =
            aterm_agent::harness::relaunch::snapshot_of_copy(&sid, &copy, &resume, shell, None);
        crate::self_claim::set_copy_for_test(&sid, Some(copy));
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
    };
    let state = aterm_tempfile::tempdir().unwrap();
    let offered =
        || crate::harness_host::with_harness_state(state.path(), || move_offered(&ctx, &proof));
    assert!(!offered(), "no host snapshot of it");
    publish(proof.pid, 1);
    assert!(!offered(), "the host holds no claim on the session");
    assert_eq!(
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us()
        ),
        Ok(true)
    );
    assert!(offered(), "CONTROL: the restart would be carried");
    publish(proof.pid, aterm_agent::harness::relaunch::NO_SHELL);
    assert!(!offered(), "no shell under it would type its relaunch");
    publish(proof.pid + 1, 1);
    assert!(!offered(), "the host's snapshot is of another process");
    publish(proof.pid, 1);
    *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "someone".to_string(),
        expires_us: crate::metrics::now_us() + 60_000_000,
        conn: None,
        hard: false,
        here: false,
        holder_pid: None,
    });
    assert!(!offered(), "another driver's lease");
    *ctx.turn_lease.lock().unwrap() = None;
    assert!(offered(), "CONTROL: the lease gone");
    // A plain restart of it already stands: the act refuses every account ask against it
    // (`account-restart-pending`), so no move is offered until it is carried.
    let plain = aterm_agent::harness::restart_stamp::Stamp {
        pid: proof.pid,
        start: proof.start.clone(),
        cause: aterm_agent::harness::restart_stamp::Cause::Asked,
        at: aterm_time::unix_secs().unwrap_or(0),
        wanted: None,
        input: None,
    };
    aterm_agent::harness::restart_stamp::write(state.path(), &sid, &plain).expect("a stamp");
    assert!(!offered(), "a restart of it stands");
    let _ = aterm_agent::harness::restart_stamp::take_if(state.path(), &sid, |_| true);
    assert!(offered(), "CONTROL: the restart carried");
    assert!(carried(&ctx, &proof, Some(state.path()), false), "CONTROL");
    assert!(
        !carried(&ctx, &proof, None, false),
        "no harness state directory would hold its stamp"
    );
    assert!(
        !carried(&ctx, &proof, Some(state.path()), true),
        "this connection gave up its restart authority"
    );
    // The launch a snapshot judges is the tab's validated copy of the proven process only.
    assert!(launch_of(&sid, &proof).is_some(), "CONTROL: its own copy");
    let reborn = crate::self_claim::Proven {
        pid: proof.pid,
        start: "later".into(),
    };
    assert!(
        launch_of(&sid, &reborn).is_none(),
        "a copy of the process before it is not this one's launch"
    );
    let other = crate::self_claim::Proven {
        pid: proof.pid + 1,
        start: proof.start.clone(),
    };
    assert!(launch_of(&sid, &other).is_none(), "nor another process's");
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    crate::self_claim::set_copy_for_test(&sid, None);
}
