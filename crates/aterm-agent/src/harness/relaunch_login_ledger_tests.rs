// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The m16 incident replayed against the upgrade's relaunch plan (plan package UB-04), from the
//! records the build that struck the row left: a row struck signed out by a wall its login did
//! not cause, its login serving all day in other tabs, is planned and admitted at the fixed
//! build's first look, once each tab's first idle point has folded the replies its process's
//! transcript already holds.

use super::*;
use aterm_json::Value;

/// The planned relaunch of the agent in tab `s-1`, which exited, on the default home's stored
/// login: a selection of that login, which the login policy admits only where row `m16`'s login
/// verdict does not read it signed out. (A relaunch that replaces a live agent on its own login
/// selects nothing, so no login verdict holds it whatever the evidence says:
/// `a_same_login_plan_carries_its_intent_where_a_selection_waits_on_the_policy_word`.)
fn planned(opts: &Opts, cwd: &str) -> Result<Plan, NoPlan> {
    let argv = words(&["claude", "--verbose"]);
    let exe = PathBuf::from("/opt/claude");
    let launch = Launch {
        session: "aaaaaaaa-0000-0000-0000-000000000016",
        resume: true,
        cwd,
        agent: 0,
        argv: &argv,
        exe: &exe,
        mode: None,
        home: None,
        homedir: None,
        login: None,
        route: accounts::AuthRoute::Stored,
        running: None,
        auth_observed: true,
        launch_env: None,
        serve: Serve::Judge,
    };
    plan(opts, &launch, 999_999, &[(999_999, 1, "zsh".into())])
}

/// One observation of `m16`'s login, written as the host writes it: in its own transaction.
fn observe(
    opts: &Opts,
    edit: impl FnOnce(&mut accounts::State, &mut accounts::Logins) -> Vec<accounts::Event>,
) -> Vec<accounts::Event> {
    account_seat(opts)
        .expect("a seat")
        .store(accounts::Cli::Claude)
        .transact_logins(now_s() as i64, &mut None, |state, logins| {
            Ok(edit(state, logins))
        })
        .expect("recorded")
}

/// A transcript row of the main chain, written at `at`: the model's own reply, or, where
/// `api_error`, Claude Code's notice of an API error in the reply's place.
fn row(at: i64, uuid: &str, api_error: bool) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at).replace('Z', ".000Z");
    let (model, error) = if api_error {
        (
            "<synthetic>",
            r#","isApiErrorMessage":true,"error":"authentication_failed""#,
        )
    } else {
        ("claude-fable-5", "")
    };
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"{uuid}","timestamp":"{stamp}"{error},"message":{{"id":"m","model":"{model}","role":"assistant","type":"message","content":[{{"type":"text","text":"Stage 3 landed."}}]}},"version":"2.1.323"}}"#
    )
}

/// What one tab's first idle point on the fixed build records of `m16` from its process's
/// transcript `tail`, as the host's turn-end point does where the row reads struck
/// (`harness_host.rs`' `login_served`, `transcript_served`): the newest model reply since the
/// process started at `started`, dated by its row ([`crate::supervise::run::served_since`]), is a
/// served reading ([`accounts::observe_served`]). A transcript whose turn ended on an API error
/// holds none.
fn first_idle_point(opts: &Opts, tail: &str, started: i64) -> Vec<accounts::Event> {
    crate::supervise::run::served_since(tail, started).map_or_else(Vec::new, |at| {
        observe(opts, |state, logins| {
            accounts::observe_served(state, logins, "m16", at)
        })
    })
}

/// THE m16 INCIDENT, REPLAYED FROM THE OLD BUILD'S RECORDS (2026-11-13): a Cloudflare 403 made
/// Claude Code print `Please run /login · API Error: 403 Attention Required! | Cloudflare` in one
/// tab, and the build then running struck row `m16` signed out. It left `state.json` holding that
/// word and its time and no ledger, and `logins.json` the login's end at the wall and a served
/// clock its idle points re-stamped since, with no turn worked to them. Its login went on serving
/// turns in the other tabs all day. On those records the upgrade's selection waits on
/// `account-policy-signed-out`: a re-stamped served clock is no evidence of service, so nothing
/// on record outranks the strike. The fixed build's first look plans the relaunch and admits its
/// intent, with no new turn served and no writer act beyond the readings: each tab's first idle
/// point folds the newest reply its process's transcript already holds since it started, dated
/// by its row, and the struck tab, whose turn ended on its API error, folds none; its wall, read
/// again, is no login wall to this build (4bde839de95), so it strikes nothing. NEGATIVE CONTROL
/// on the same records: a real login wall first sighted after the last reply is the newest
/// evidence, and the same plan waits on `account-policy-signed-out`.
#[test]
fn the_m16_incident_plans_and_admits_the_upgrade_on_the_first_look() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let opts = default_home_m16(dir.path());
    let cwd = dir.path().to_str().expect("utf-8").to_owned();
    let now = now_s() as i64;
    let hour = 3600;
    let started = now - 10 * hour;
    let wall = now - 9 * hour;
    observe(&opts, |state, logins| {
        state.home_accounts.extend(["m16".into(), "work-2".into()]);
        let m16 = state.accounts.entry("m16".into()).or_default();
        m16.auth = accounts::Auth::SignedOut;
        m16.auth_at = Some(wall);
        let binding = logins.labels.entry("m16".into()).or_default();
        binding.ended_at = Some(wall);
        binding.served_at = Some(now - 60);
        Vec::new()
    });
    let store = account_seat(&opts)
        .expect("a seat")
        .store(accounts::Cli::Claude);
    let doc: Value =
        aterm_json::from_str(&std::fs::read_to_string(store.state_file()).expect("state.json"))
            .expect("json");
    let m16 = doc.get("accounts").and_then(|a| a.get("m16")).expect("m16");
    assert_eq!(
        (
            m16.get("auth").and_then(Value::as_str),
            m16.get("login").is_none()
        ),
        (Some(accounts::Auth::SignedOut.as_str()), true),
        "PRECONDITION: the old build's word, and no ledger"
    );
    match planned(&opts, &cwd) {
        Err(NoPlan::Account(refusal)) => assert_eq!(
            refusal.kind.word(),
            "account-policy-signed-out",
            "{refusal:?}"
        ),
        Ok(plan) => panic!(
            "PRECONDITION: a re-stamped served clock planned: {}",
            plan.line
        ),
        Err(NoPlan::Wait(word)) => panic!("PRECONDITION: wait {word}"),
        Err(NoPlan::Noted { word, why }) => panic!("PRECONDITION: wait {word}: {why}"),
        Err(NoPlan::Refused { why, .. }) => panic!("PRECONDITION: refused {why}"),
    }
    let edge = "Please run /login · API Error: 403 Attention Required! | Cloudflare";
    assert_ne!(
        aterm_phase::classify_wall(edge),
        Some(alab_policy::vocab::WallKind::Auth),
        "the struck tab's wall is no login wall to the fixed build"
    );
    let struck_tab = [row(wall - 600, "r-1", false), row(wall, "e-1", true)].join("\n");
    assert!(
        first_idle_point(&opts, &struck_tab, started).is_empty(),
        "the struck tab's turn ended on its API error: no service since"
    );
    for tail in [
        [
            row(now - 8 * hour, "r-2", false),
            row(now - 20 * 60, "r-3", false),
        ]
        .join("\n"),
        row(now - 4 * hour, "r-4", false),
    ] {
        first_idle_point(&opts, &tail, started);
    }
    let plan = match planned(&opts, &cwd) {
        Ok(plan) => plan,
        Err(NoPlan::Account(refusal)) => panic!("account: {refusal:?}"),
        Err(NoPlan::Wait(word)) => panic!("wait: {word}"),
        Err(NoPlan::Noted { word, why }) => panic!("wait: {word}: {why}"),
        Err(NoPlan::Refused { why, .. }) => panic!("refused: {why}"),
    };
    let intent = plan
        .command
        .and_then(|command| command.account_intent)
        .expect("an account intent");
    let crate::harness::accounts_pool::ExecutionIntent::Stored { owner, .. } = &intent else {
        panic!("a stored intent: {intent:?}");
    };
    assert_eq!(owner.as_deref(), Some("m16"));
    if let Err(refusal) = account_execution(&opts, Some(&intent)) {
        panic!("the plan's intent is admitted: {refusal:?}");
    }
    let lost = "Please run /login · API Error: 401 Cloudflare";
    assert_eq!(
        aterm_phase::classify_wall(lost),
        Some(alab_policy::vocab::WallKind::Auth),
        "PRECONDITION: a real login wall"
    );
    let mut new_process = accounts::WallSighting::<u32>::default();
    observe(&opts, |state, logins| {
        let at = new_process.sighted(&17, now - 30, |held, seen| held == seen);
        accounts::note_login_end(state, logins, "m16", at)
    });
    match planned(&opts, &cwd) {
        Err(NoPlan::Account(refusal)) => assert_eq!(
            refusal.kind.word(),
            "account-policy-signed-out",
            "{refusal:?}"
        ),
        Ok(plan) => panic!("CONTROL: a newer wall plans: {}", plan.line),
        Err(NoPlan::Wait(word)) => panic!("CONTROL: wait {word}"),
        Err(NoPlan::Noted { word, why }) => panic!("CONTROL: wait {word}: {why}"),
        Err(NoPlan::Refused { why, .. }) => panic!("CONTROL: refused {why}"),
    }
}
