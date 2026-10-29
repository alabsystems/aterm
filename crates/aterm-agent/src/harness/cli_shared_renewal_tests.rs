// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::shared_renewal::{self, Authority, Grant};
use super::*;
use std::cell::Cell;

const MAIL: &str = "owner@example.test";
struct Approval {
    live: Cell<bool>,
    generation: Cell<u64>,
}
impl Authority for Approval {
    fn prepare(&self, _: &accounts::Row) -> Result<Grant, String> {
        if !self.live.get() {
            return Err("root stopped preparation".into());
        }
        Ok(Grant {
            source: accounts::shared::Source {
                network: [1; 32],
                account: [2; 32],
            },
            decision: [self.generation.get() as u8; 32],
            generation: self.generation.get(),
            configuration: [4; 32],
            identity: MAIL.into(),
            expected_principal: profile::principal_id("provider-uuid"),
            required_browser_profile: None,
        })
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        if self.prepare(row)? != *grant {
            return Err("root decision changed".into());
        }
        Ok(())
    }
}

struct Host<'a> {
    approval: &'a Approval,
    calls: Cell<usize>,
    stop: bool,
    uuid: &'static str,
}
impl AccountsHost for Host<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("shared renewal must inspect only staging")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("shared renewal uses named OAuth")
    }
    fn now(&self) -> i64 {
        1_700_000_010
    }
    fn renew_flow(&self, home: &str, email: &str, _: &RenewBrowser) -> Result<i32, String> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(email, MAIL);
        std::fs::write(
            Path::new(home).join(".claude.json"),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"{MAIL}","accountUuid":"{}"}}}}"#,
                self.uuid
            ),
        )
        .unwrap();
        if self.stop {
            self.approval.live.set(false);
        }
        Ok(0)
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        Ok(Captured {code:Some(0),stdout:format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{MAIL}","subscriptionType":"max"}}"#).into_bytes()})
    }

    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        profile::identity_in_home(Path::new(home), email)
    }
}

fn fixture(tag: &str) -> (Tmp, Env) {
    let tmp = tmp_dir(tag);
    let mut env = env_at(&tmp);
    env.now = 1_700_000_000;
    let row = accounts::Row {
        label: "shared".into(),
        email: Some(MAIL.into()),
        ..accounts::Row::default()
    };
    std::fs::write(
        env.accounts_roster().unwrap(),
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    (tmp, env)
}
fn shared_run(env: &Env, host: &Host<'_>, request: u8) -> ExitCode {
    shared_renewal::run(
        env,
        host,
        "shared",
        [request; 16],
        host.approval,
        &mut Vec::new(),
        &mut Vec::new(),
    )
}

fn login_homes(env: &Env) -> std::collections::BTreeSet<PathBuf> {
    let entries = match std::fs::read_dir(env.accounts_store().logins_dir()) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Default::default(),
        Err(err) => panic!("cannot inspect login homes: {err}"),
    };
    entries.map(|entry| entry.unwrap().path()).collect()
}

#[test]
fn a_shared_grant_is_consumed_once_and_ordinary_renewal_cannot_bypass_its_pin() {
    let (_tmp, env) = fixture("shared-renew-once");
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: false,
        uuid: "provider-uuid",
    };
    assert_eq!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 1);
    let homes = login_homes(&env);
    assert_eq!(homes.len(), 1);
    assert_ne!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    assert_eq!(login_homes(&env), homes);
    assert_ne!(shared_run(&env, &host, 2), ExitCode::SUCCESS);
    assert_eq!(login_homes(&env), homes);
    let form = AccountsCmd::Renew {
        labels: vec!["shared".into()],
        due: false,
        on: Some(vec![]),
    };
    assert_ne!(
        run_accounts(&form, &env, &host, &mut Vec::new(), &mut Vec::new()),
        ExitCode::SUCCESS
    );
    assert_eq!(host.calls.get(), 1);
    let logins = env.accounts_store().load_logins(env.now).unwrap().0;
    let attempt = logins
        .shared
        .latest(accounts::shared::Source {
            network: [1; 32],
            account: [2; 32],
        })
        .unwrap();
    assert!(matches!(
        attempt.outcome,
        accounts::shared::Outcome::Published { generation: 2 }
    ));
    assert_eq!(logins.get("shared").unwrap().renewal_generation, 2);

    approval.generation.set(2);
    assert_eq!(shared_run(&env, &host, 3), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 2);
    let homes = login_homes(&env);
    assert_eq!(homes.len(), 2);
    approval.generation.set(1);
    assert_ne!(shared_run(&env, &host, 4), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 2);
    assert_eq!(login_homes(&env), homes);
}

#[test]
fn stopping_during_provider_io_keeps_staging_and_blocks_binding_and_nonce_replay() {
    let (_tmp, env) = fixture("shared-renew-stopped");
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: true,
        uuid: "provider-uuid",
    };
    assert_ne!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    let renamed = accounts::Row {
        label: "renamed-local-account".into(),
        email: Some("new-address@example.test".into()),
        ..accounts::Row::default()
    };
    let logins = env.accounts_store().load_logins(env.now).unwrap().0;
    assert!(
        shared_renewal::unmanaged(
            &logins,
            &renamed,
            profile::principal_id("provider-uuid"),
            true,
        )
        .is_err()
    );
    assert!(logins.get("shared").unwrap().dir.is_none());
    let attempt = logins
        .shared
        .latest(accounts::shared::Source {
            network: [1; 32],
            account: [2; 32],
        })
        .unwrap();
    assert_eq!(attempt.outcome, accounts::shared::Outcome::Unknown);
    assert!(
        Path::new(&attempt.intent.staging)
            .join(".claude.json")
            .is_file()
    );
    approval.live.set(true);
    let homes = login_homes(&env);
    assert_ne!(shared_run(&env, &host, 2), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 1);
    assert_eq!(login_homes(&env), homes);
}

#[test]
fn a_matching_email_cannot_publish_a_different_provider_uuid() {
    let (_tmp, env) = fixture("shared-renew-principal");
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: false,
        uuid: "another-provider-uuid",
    };
    assert_ne!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    assert!(
        env.accounts_store()
            .load_logins(env.now)
            .unwrap()
            .0
            .get("shared")
            .unwrap()
            .dir
            .is_none()
    );
    assert_eq!(host.calls.get(), 1);
    let state = env.accounts_store().load_logins(env.now).unwrap().0.shared;
    assert!(
        state
            .principal_source(profile::principal_id("provider-uuid").unwrap())
            .is_some()
    );
    assert!(
        state
            .principal_source(profile::principal_id("another-provider-uuid").unwrap())
            .is_none()
    );
}

struct PrincipalOverride<'a>(&'a Approval, Option<[u8; 32]>);
impl Authority for PrincipalOverride<'_> {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        let mut grant = self.0.prepare(row)?;
        grant.expected_principal = self.1;
        Ok(grant)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        if self.prepare(row)? != *grant {
            return Err("root decision changed".into());
        }
        Ok(())
    }
}

#[test]
fn a_changed_approved_uuid_is_refused_before_staging_or_provider_entry() {
    let (_tmp, env) = fixture("shared-renew-changed-principal");
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: false,
        uuid: "provider-uuid",
    };
    assert_eq!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    let homes = login_homes(&env);
    approval.generation.set(2);
    let incompatible = PrincipalOverride(&approval, profile::principal_id("another-provider-uuid"));
    assert_ne!(
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [2; 16],
            &incompatible,
            &mut Vec::new(),
            &mut Vec::new()
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(host.calls.get(), 1);
    assert_eq!(login_homes(&env), homes);
    assert_eq!(shared_run(&env, &host, 3), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 2);
}

#[test]
fn explicit_bootstrap_can_learn_the_provider_uuid_from_a_successful_login() {
    let (_tmp, env) = fixture("shared-renew-bootstrap");
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: false,
        uuid: "provider-uuid",
    };
    let authority = PrincipalOverride(&approval, None);
    assert_eq!(
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [1; 16],
            &authority,
            &mut Vec::new(),
            &mut Vec::new()
        ),
        ExitCode::SUCCESS
    );
    let logins = env.accounts_store().load_logins(env.now).unwrap().0;
    assert_eq!(host.calls.get(), 1);
    assert!(logins.get("shared").unwrap().dir.is_some());
    assert!(
        logins
            .shared
            .principal_source(profile::principal_id("provider-uuid").unwrap())
            .is_some()
    );
}

#[test]
fn refused_root_permission_runs_no_provider_and_leaves_no_shared_pin() {
    let (_tmp, env) = fixture("shared-renew-refused");
    let approval = Approval {
        live: Cell::new(false),
        generation: Cell::new(1),
    };
    let host = Host {
        approval: &approval,
        calls: Cell::new(0),
        stop: false,
        uuid: "provider-uuid",
    };
    assert_ne!(shared_run(&env, &host, 1), ExitCode::SUCCESS);
    assert_eq!(host.calls.get(), 0);
    assert!(
        env.accounts_store()
            .load_logins(env.now)
            .unwrap()
            .0
            .shared
            .is_empty()
    );
}

struct ScenarioHost<'a> {
    base: Host<'a>,
    case: i64,
    path: PathBuf,
}
impl AccountsHost for ScenarioHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, dir: &str) -> Result<Captured, String> {
        self.base.auth_status(dir)
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        self.base.login(dir)
    }
    fn now(&self) -> i64 {
        self.base.now()
    }
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        let result = self.base.renew_flow(home, email, browser);
        match self.case {
            5 => {
                std::fs::write(&self.path,"[[account]]\nlabel='shared'\nclass='dedicated'\nemail='changed@example.test'\n").unwrap();
            }
            6 => panic!("injected crash after provider entry"),
            7 => self.base.approval.generation.set(2),
            _ => {}
        }
        result
    }
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        if self.case == 8 {
            std::fs::write(
                Path::new(home).join(".claude.json"),
                format!(r#"{{"oauthAccount":{{"emailAddress":"{MAIL}"}}}}"#),
            )
            .unwrap();
        }
        self.base.status_in_home(home)
    }

    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        self.base.claude_identity_in_home(home, email)
    }
}

struct ExactGrant<'a>(&'a Approval);
impl Authority for ExactGrant<'_> {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        if self.0.generation.get() != 1 {
            return Err("the explicitly requested decision changed".into());
        }
        self.0.prepare(row)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        self.0.current(grant, row)
    }
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "Good" => 1,
        "StoppedBefore" => 2,
        "StoppedDuring" => 3,
        "WrongPrincipal" => 4,
        "ChangedRow" => 5,
        "Crash" => 6,
        "ChangedHead" => 7,
        "MissingPrincipal" => 8,
        _ => state["case"],
    };
    let (_tmp, env) = fixture("shared-renew-model");
    let approval = Approval {
        live: Cell::new(case != 2),
        generation: Cell::new(1),
    };
    let host = ScenarioHost {
        base: Host {
            approval: &approval,
            calls: Cell::new(0),
            stop: case == 3,
            uuid: if case == 4 {
                "wrong-provider-uuid"
            } else {
                "provider-uuid"
            },
        },
        case,
        path: env.accounts_roster().unwrap(),
    };
    let authority = ExactGrant(&approval);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [1; 16],
            &authority,
            &mut Vec::new(),
            &mut Vec::new(),
        )
    }));
    let entered = host.base.calls.get() as i64;
    let store = env.accounts_store();
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("entered", entered);
    next.insert("ever", i64::from(entered > 0));
    next.insert("homes", login_homes(&env).len() as i64);
    match action {
        "Replay" => {
            if case != 2 {
                approval.live.set(true);
                approval.generation.set(1);
            }
            let _ = shared_renewal::run(
                &env,
                &host,
                "shared",
                [2; 16],
                &authority,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            next.insert("replayed", 1);
            next.insert("after", host.base.calls.get() as i64);
            next.insert("after_homes", login_homes(&env).len() as i64);
        }
        "MetadataEdit" => {
            let _ = store.transact_logins_within(
                &accounts::LOCK_LADDER,
                env.now,
                &mut None,
                |_, logins| {
                    logins.shared = accounts::shared::State::default();
                    Ok(Vec::new())
                },
            );
            next.insert("edited", 1);
        }
        "Restart" => {
            next.insert("restarted", 1);
        }
        _ => {}
    }
    let mut logins = store.load_logins(env.now).unwrap().0;
    if action == "Restart" {
        logins = accounts::Logins::from_json(&logins.to_json(), env.now).unwrap();
    }
    let source = accounts::shared::Source {
        network: [1; 32],
        account: [2; 32],
    };
    next.insert("pinned", i64::from(!logins.shared.is_empty()));
    next.insert(
        "consumed",
        i64::from(logins.shared.latest(source).is_some()),
    );
    next.insert(
        "reserved",
        i64::from(
            logins
                .shared
                .principal_source(profile::principal_id("provider-uuid").unwrap())
                == Some(source),
        ),
    );
    next.insert(
        "bound",
        i64::from(logins.get("shared").is_some_and(|b| b.dir.is_some())),
    );
    next
}

#[test]
fn guarded_workflow_conforms_and_reentry_or_lost_custody_is_a_negative_control() {
    let model = aterm_models_harness::shared_renewal_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 8);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let good = transition(&model.init_state(), "Good");
    let mut bad = transition(&good, "Replay");
    bad.insert("after", 2);
    assert!(!model.check_invariant("NoReplay", &bad));
    let mut bad = transition(&good, "Replay");
    bad.insert("after_homes", 2);
    assert!(!model.check_invariant("NoReplayStaging", &bad));
    let mut bad = transition(&good, "MetadataEdit");
    bad.insert("pinned", 0);
    assert!(!model.check_invariant("RetainPin", &bad));
    let mut bad = transition(&model.init_state(), "StoppedDuring");
    bad.insert("reserved", 0);
    assert!(!model.check_invariant("ApprovedPrincipalRetained", &bad));
}

struct BrowserPolicy<'a> {
    approval: &'a Approval,
    required: bool,
}

impl Authority for BrowserPolicy<'_> {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        let mut grant = self.approval.prepare(row)?;
        grant.required_browser_profile = self.required.then(|| "Required profile".into());
        Ok(grant)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        if self.prepare(row)? != *grant {
            return Err("browser requirement changed".into());
        }
        Ok(())
    }
}

pub(crate) fn browser_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (case, required, browser) = match action {
        "OptionalDefault" => (1, false, RenewBrowser::Default),
        "RequiredDefault" => (2, true, RenewBrowser::Default),
        "OptionalRemote" => (3, false, RenewBrowser::Remote { opener: None }),
        "RequiredRemote" => (4, true, RenewBrowser::Remote { opener: None }),
        "OptionalProfile" => (
            5,
            false,
            RenewBrowser::Profile {
                opener: PathBuf::from("test-opener"),
                profile: "Other profile".into(),
            },
        ),
        "WrongProfile" => (
            6,
            true,
            RenewBrowser::Profile {
                opener: PathBuf::from("test-opener"),
                profile: "Other profile".into(),
            },
        ),
        "RequiredProfile" => (
            7,
            true,
            RenewBrowser::Profile {
                opener: PathBuf::from("test-opener"),
                profile: "Required profile".into(),
            },
        ),
        _ => panic!("unknown browser action {action}"),
    };
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let authority = BrowserPolicy {
        approval: &approval,
        required,
    };
    let row = accounts::Row {
        label: "shared".into(),
        email: Some(MAIL.into()),
        browser: required.then(|| "Required profile".into()),
        ..accounts::Row::default()
    };
    let mut guard = shared_renewal::Guard::Shared {
        authority: &authority,
        request: [1; 16],
        grant: None,
        token: None,
    };
    let accepted = guard
        .preflight(&row, &accounts::Logins::default(), &browser)
        .is_ok();
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("required", i64::from(required));
    next.insert("matching", i64::from(case == 7));
    next.insert("accepted", i64::from(accepted));
    next
}

#[test]
fn approved_browser_routes_conform_and_downgrading_or_disabling_them_is_detected() {
    let model = aterm_models_harness::shared_browser_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 5);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = browser_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut downgraded = browser_transition(&model.init_state(), "RequiredDefault");
    downgraded.insert("accepted", 1);
    assert!(!model.check_invariant("EnforceRequired", &downgraded));
    let mut disabled = browser_transition(&model.init_state(), "RequiredProfile");
    disabled.insert("accepted", 0);
    assert!(!model.check_invariant("AllowSupported", &disabled));
}

struct BeginAuthority<'a> {
    approval: Approval,
    store: &'a accounts::Store,
    locked: Cell<bool>,
    unavailable: Cell<bool>,
    observation: Cell<usize>,
}

impl Authority for BeginAuthority<'_> {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        if self.unavailable.get() {
            return Err("authority observation is unavailable".into());
        }
        self.approval.prepare(row)
    }

    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        let probe = self.store.try_acquire().map_err(|e| e.to_string())?;
        self.locked.set(probe.is_none());
        drop(probe);
        if self.prepare(row)? != *grant {
            return Err("root decision changed".into());
        }
        Ok(())
    }
}

fn begin_case(case: i64) -> [i64; 4] {
    let (_tmp, env) = fixture("shared-begin-authority");
    let store = env.accounts_store();
    let path = env.accounts_roster().unwrap();
    let records = store.read_account_records(Some(&path), env.now).unwrap();
    let row = records.roster.row("shared").unwrap();
    let authority = BeginAuthority {
        approval: Approval {
            live: Cell::new(true),
            generation: Cell::new(1),
        },
        store: &store,
        locked: Cell::new(false),
        unavailable: Cell::new(false),
        observation: Cell::new(0),
    };
    let mut guard = shared_renewal::Guard::Shared {
        authority: &authority,
        request: [1; 16],
        grant: None,
        token: None,
    };
    guard
        .preflight(row, &records.logins, &RenewBrowser::Default)
        .unwrap();
    match case {
        2 => authority.approval.live.set(false),
        3 => authority.approval.generation.set(2),
        4 => authority.unavailable.set(true),
        5 => authority.observation.set(1),
        _ => {}
    }
    let images = || {
        [store.state_file(), store.logins_file(), store.ledger_path()]
            .map(|path| std::fs::read(path).ok())
    };
    let before = images();
    let staging = store.claim_staging_dir("shared").unwrap();
    let result = guard.begin(
        &store,
        accounts::RenewalPolicy {
            roster_path: &path,
            roster: &records.raw_roster,
            row,
            logins: &records.logins,
        },
        staging.to_str().unwrap(),
        env.now,
    );
    if result.is_err() {
        assert_eq!(
            images(),
            before,
            "a refused begin writes no account records"
        );
    }
    let logins = store.load_logins(env.now).unwrap().0;
    let source = accounts::shared::Source {
        network: [1; 32],
        account: [2; 32],
    };
    let token = match guard {
        shared_renewal::Guard::Shared { token, .. } => token.is_some(),
        shared_renewal::Guard::Local => unreachable!(),
    };
    assert_eq!(result.is_ok(), token);
    if case == 5 {
        assert_eq!(authority.observation.get(), 1);
    }
    [
        i64::from(logins.shared.latest(source).is_some()),
        i64::from(
            logins
                .shared
                .principal_source(profile::principal_id("provider-uuid").unwrap())
                == Some(source),
        ),
        i64::from(token),
        i64::from(authority.locked.get()),
    ]
}

#[test]
fn shared_begin_observes_authority_under_the_account_writer() {
    assert_eq!(begin_case(1), [1, 1, 1, 1]);
}

#[test]
fn shared_begin_refusal_writes_no_consumption_or_custody() {
    for case in [2, 3, 4] {
        assert_eq!(begin_case(case), [0, 0, 0, 1], "case {case}");
    }
}

pub(crate) fn begin_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "Current" => 1,
        "Stopped" => 2,
        "ChangedHead" => 3,
        "Unavailable" => 4,
        "Unrelated" => 5,
        _ => panic!("unknown begin action {action}"),
    };
    let [consumed, reserved, token, locked] = begin_case(case);
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("locked", locked);
    next.insert("consumed", consumed);
    next.insert("reserved", reserved);
    next.insert("token", token);
    next
}

#[test]
fn shared_begin_conforms_at_the_durable_authority_boundary() {
    let model = aterm_models_harness::shared_begin_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 6);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = begin_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    for (action, fields, law) in [
        ("Current", vec![("locked", 0)], "AuthorityWhileLocked"),
        ("Stopped", vec![("consumed", 1)], "OnlyCurrentBegin"),
        (
            "Current",
            vec![("consumed", 0), ("reserved", 0), ("token", 0)],
            "EligibleProgress",
        ),
        ("Current", vec![("token", 0)], "AtomicCustody"),
    ] {
        let mut bad = begin_transition(&model.init_state(), action);
        for (field, value) in fields {
            bad.insert(field, value);
        }
        assert!(!model.check_invariant(law, &bad));
    }
}

fn snapshot_case(shared: bool, case: i64) -> [(&'static str, i64); 8] {
    let (tmp, env) = fixture("renewal-begin-snapshot");
    let store = env.accounts_store();
    let path = env.accounts_roster().unwrap();
    let prior = tmp.path().join("prior-login");
    std::fs::create_dir(&prior).unwrap();
    let prior = prior.to_str().unwrap();
    // Prime the retained path so another same-path renewal changes the generation
    // without changing the effective row, including its login history.
    for at in [env.now - 20, env.now - 19] {
        store
            .transact_logins(at, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state, logins, "shared", None, prior, None, at,
                ))
            })
            .unwrap();
    }
    let records = store.read_account_records(Some(&path), env.now).unwrap();
    let row = records.roster.row("shared").unwrap();
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let mut guard = if shared {
        shared_renewal::Guard::Shared {
            authority: &approval,
            request: [1; 16],
            grant: None,
            token: None,
        }
    } else {
        shared_renewal::Guard::Local
    };
    guard
        .preflight(row, &records.logins, &RenewBrowser::Default)
        .unwrap();
    let staging = store.claim_staging_dir("shared").unwrap();
    let mut committed = None;
    let mut excluded = false;
    let mut moved = false;
    let result = guard.begin_with(
        &store,
        accounts::RenewalPolicy {
            roster_path: &path,
            roster: &records.raw_roster,
            row,
            logins: &records.logins,
        },
        staging.to_str().unwrap(),
        env.now,
        || {
            committed = Some(store.load_logins(env.now).unwrap().0);
            let competitor = store.try_acquire().unwrap();
            excluded = competitor.is_none();
            if let Some(held) = competitor
                && matches!(case, 1 | 2)
            {
                let label = if case == 1 { "shared" } else { "unrelated" };
                store
                    .transact_logins_held(&held, env.now, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state, logins, label, None, prior, None, env.now,
                        ))
                    })
                    .unwrap();
            }
            let current = store.load_logins(env.now).unwrap().0;
            assert_eq!(
                accounts::bound(&records.raw_roster, &current).row("shared"),
                Some(row)
            );
            if case == 4 {
                std::fs::rename(
                    store.logins_file(),
                    store.logins_file().with_extension("retained"),
                )
                .unwrap();
                moved = true;
            }
        },
    );
    if moved {
        std::fs::rename(
            store.logins_file().with_extension("retained"),
            store.logins_file(),
        )
        .unwrap();
    }
    let exact = match &result {
        Ok(returned) => Some(returned) == committed.as_ref(),
        Err(_) => true,
    };
    let released = store.try_acquire().unwrap().is_some();
    let mut stale = true;
    if let Ok(returned) = &result {
        store
            .transact_logins(env.now + 1, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "shared",
                    None,
                    prior,
                    None,
                    env.now + 1,
                ))
            })
            .unwrap();
        let current = store.load_logins(env.now + 1).unwrap().0;
        assert_eq!(
            accounts::bound(&records.raw_roster, &current).row("shared"),
            Some(row)
        );
        assert!(
            current.get("shared").unwrap().renewal_generation
                > returned.get("shared").unwrap().renewal_generation
        );
        stale = store
            .transact_renewal_within(
                &[],
                env.now + 1,
                &mut None,
                accounts::RenewalPolicy {
                    roster_path: &path,
                    roster: &records.raw_roster,
                    row,
                    logins: returned,
                },
                |_, _| Ok(Vec::new()),
            )
            .is_err();
    }
    let committed = committed.unwrap();
    let source = accounts::shared::Source {
        network: [1; 32],
        account: [2; 32],
    };
    let consumed = committed.shared.latest(source).is_some();
    let token = matches!(guard, shared_renewal::Guard::Shared { token: Some(_), .. });
    [
        ("phase", 1),
        ("returned", i64::from(result.is_ok())),
        ("exact", i64::from(exact)),
        ("excluded", i64::from(excluded)),
        ("released", i64::from(released)),
        ("stale", i64::from(stale)),
        ("consumed", i64::from(consumed)),
        ("token", i64::from(token)),
    ]
}

#[test]
fn local_begin_returns_its_own_commit() {
    let result = snapshot_case(false, 1);
    assert_eq!(result.iter().find(|(key, _)| *key == "exact").unwrap().1, 1);
}

#[test]
fn shared_begin_returns_its_own_commit() {
    let result = snapshot_case(true, 1);
    assert_eq!(result.iter().find(|(key, _)| *key == "exact").unwrap().1, 1);
}

pub(crate) fn snapshot_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherMode" => {
            next.insert("shared", 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Begin" => {
            for (key, value) in snapshot_case(state["shared"] == 1, state["case"]) {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn renewal_snapshot_conforms_and_detects_parent_substitution() {
    let model = aterm_models_harness::renewal_snapshot_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 16);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = snapshot_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("shared", 1);
    let after = snapshot_transition(&state, "Begin");
    for (field, value, law) in [
        ("exact", 0, "CommittedParent"),
        ("excluded", 0, "WriterExclusion"),
        ("released", 0, "WriterReleased"),
        ("stale", 0, "NewerParentRefused"),
        ("consumed", 0, "ConsumptionRetained"),
        ("token", 0, "TokenRetained"),
        ("returned", 0, "ResultPolicy"),
    ] {
        let mut bad = after.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn renewal_read_failure_keeps_its_consumed_grant_and_releases_the_writer() {
    for shared in [false, true] {
        let result = snapshot_case(shared, 4);
        let field = |key| result.iter().find(|(name, _)| *name == key).unwrap().1;
        assert_eq!(field("returned"), 0);
        assert_eq!(field("consumed"), i64::from(shared));
        assert_eq!(field("token"), i64::from(shared));
        assert_eq!(field("excluded"), 1);
        assert_eq!(field("released"), 1);
    }
}

struct CheckpointFailureHost<'a> {
    base: Host<'a>,
    ledger: PathBuf,
    after_checkpoint: bool,
    obstructed: Cell<bool>,
}

impl AccountsHost for CheckpointFailureHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, dir: &str) -> Result<Captured, String> {
        self.base.auth_status(dir)
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        self.base.login(dir)
    }
    fn now(&self) -> i64 {
        self.base.now()
    }
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        self.base.renew_flow(home, email, browser)
    }
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        let checked = self.base.status_in_home(home)?;
        if !self.obstructed.replace(true) {
            if self.after_checkpoint {
                // The ledger is read only after the edit has saved its checkpoint.
                std::fs::rename(&self.ledger, self.ledger.with_extension("retained")).unwrap();
                std::fs::create_dir(&self.ledger).unwrap();
            } else {
                std::fs::create_dir(Path::new(home).join(accounts::RENEWAL_CHECKPOINT_FILE))
                    .unwrap();
            }
        }
        Ok(checked)
    }

    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        self.base.claude_identity_in_home(home, email)
    }
}

fn checkpoint_case(prior: bool, after_checkpoint: bool) -> [[(&'static str, i64); 9]; 3] {
    let (_tmp, env) = fixture("shared-checkpoint-failure");
    let store = env.accounts_store();
    let prior_home = store.logins_dir().join("prior");
    if prior {
        std::fs::create_dir_all(&prior_home).unwrap();
        std::fs::write(prior_home.join(".claude.json"), "{}").unwrap();
        store
            .transact_logins(env.now - 30, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "shared",
                    None,
                    prior_home.to_str().unwrap(),
                    None,
                    env.now - 30,
                ))
            })
            .unwrap();
    }
    let approval = Approval {
        live: Cell::new(true),
        generation: Cell::new(1),
    };
    let host = CheckpointFailureHost {
        base: Host {
            approval: &approval,
            calls: Cell::new(0),
            stop: false,
            uuid: "provider-uuid",
        },
        ledger: store.ledger_path(),
        after_checkpoint,
        obstructed: Cell::new(false),
    };
    let mut err = Vec::new();
    assert_ne!(
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [1; 16],
            &approval,
            &mut Vec::new(),
            &mut err
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(host.base.calls.get(), 1);
    let source = accounts::shared::Source {
        network: [1; 32],
        account: [2; 32],
    };
    let pending = store.load_logins(env.now).unwrap().0;
    let attempt = pending.shared.latest(source).unwrap();
    assert_eq!(attempt.outcome, accounts::shared::Outcome::Pending);
    assert_eq!(
        pending.get("shared").unwrap().dir.as_deref(),
        prior.then(|| prior_home.to_str().unwrap())
    );
    let staging = Path::new(&attempt.intent.staging);
    let credential = std::fs::read(staging.join(".claude.json")).unwrap();
    let snapshot = |phase, logins: &accounts::Logins, once, same_refused| {
        let attempt = logins.shared.latest(source).unwrap();
        [
            ("phase", phase),
            ("entered", host.base.calls.get() as i64),
            ("consumed", attempt.intent.generation as i64),
            (
                "pending",
                i64::from(attempt.outcome == accounts::shared::Outcome::Pending),
            ),
            (
                "published",
                i64::from(matches!(
                    attempt.outcome,
                    accounts::shared::Outcome::Published { .. }
                )),
            ),
            (
                "bound",
                i64::from(logins.get("shared").unwrap().dir.is_some()),
            ),
            (
                "kept",
                i64::from(
                    std::fs::read(staging.join(".claude.json")).ok().as_ref() == Some(&credential),
                ),
            ),
            ("once", i64::from(once)),
            ("same_refused", i64::from(same_refused)),
        ]
    };
    assert_eq!(
        accounts::read_renewal_checkpoint(
            staging,
            &std::fs::canonicalize(staging).unwrap(),
            host.now()
        )
        .is_some(),
        after_checkpoint,
        "{}",
        String::from_utf8_lossy(&err)
    );
    let same_refused = pending
        .shared
        .check_preparation(source, 1, profile::principal_id("provider-uuid"))
        .is_err();
    assert!(same_refused);
    let failed = snapshot(1, &pending, true, same_refused);
    if after_checkpoint {
        std::fs::rename(&host.ledger, host.ledger.with_extension("obstruction")).unwrap();
        std::fs::rename(host.ledger.with_extension("retained"), &host.ledger).unwrap();
    }
    approval.live.set(false);
    let recover = || {
        crate::harness::accounts_sync::recover_staging_renewals_for(
            Cli::Claude,
            &store,
            &env.accounts_roster().unwrap(),
            host.now(),
        )
    };
    assert_eq!(recover(), after_checkpoint);
    let recovered = store.load_logins(host.now()).unwrap().0;
    assert_eq!(host.base.calls.get(), 1);
    let images = || {
        [
            env.accounts_roster().unwrap(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.state_witness_file(),
            store.logins_witness_file(),
            store.wake_path(),
            store.dir.join("sync-commit.json"),
            staging.join(accounts::RENEWAL_CHECKPOINT_FILE),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let recovered_images = images();
    let once = !recover() && images() == recovered_images;
    assert!(once);
    let recovered_state = snapshot(2, &recovered, once, same_refused);
    if after_checkpoint {
        assert_eq!(
            recovered.get("shared").unwrap().dir.as_deref(),
            staging.to_str()
        );
        assert_eq!(
            recovered.shared.latest(source).unwrap().outcome,
            accounts::shared::Outcome::Published {
                generation: pending.get("shared").unwrap().renewal_generation + 1
            }
        );
    } else {
        assert_eq!(recovered, pending);
    }
    approval.live.set(true);
    assert_ne!(
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [2; 16],
            &approval,
            &mut Vec::new(),
            &mut Vec::new()
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(host.base.calls.get(), 1);
    approval.generation.set(2);
    assert_eq!(
        shared_renewal::run(
            &env,
            &host,
            "shared",
            [3; 16],
            &approval,
            &mut Vec::new(),
            &mut Vec::new()
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(host.base.calls.get(), 2);
    let final_logins = store.load_logins(host.now()).unwrap().0;
    assert_ne!(
        final_logins.get("shared").unwrap().dir.as_deref(),
        staging.to_str()
    );
    assert!(staging.join(".claude.json").is_file());
    [
        failed,
        recovered_state,
        snapshot(3, &final_logins, once, same_refused),
    ]
}

#[test]
fn shared_checkpoint_failure_preserves_consumption_and_recovers_only_completed_evidence() {
    for after_checkpoint in [false, true] {
        for prior in [false, true] {
            let _ = checkpoint_case(prior, after_checkpoint);
        }
    }
}

pub(crate) fn checkpoint_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "PriorBinding" => {
            next.insert("prior", 1);
            next.insert("bound", 1);
        }
        "AfterCheckpoint" => {
            next.insert("fault", 1);
        }
        "Fail" | "Recover" | "Supersede" => {
            let snapshots = checkpoint_case(state["prior"] == 1, state["fault"] == 1);
            let index = match action {
                "Fail" => 0,
                "Recover" => 1,
                _ => 2,
            };
            for (key, value) in snapshots[index] {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn shared_checkpoint_transitions_conform_and_lost_or_replayed_results_are_detected() {
    let model = aterm_models_harness::shared_checkpoint_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 16);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = checkpoint_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for after_checkpoint in [0, 1] {
        let mut initial = model.init_state();
        initial.insert("fault", after_checkpoint);
        let failed = checkpoint_transition(&initial, "Fail");
        let recovered = checkpoint_transition(&failed, "Recover");
        let superseded = checkpoint_transition(&recovered, "Supersede");
        for (state, field, value, law) in [
            (&failed, "consumed", 0, "Consumed"),
            (&failed, "pending", 0, "FailedParent"),
            (&failed, "same_refused", 0, "NoReplay"),
            (
                &recovered,
                "published",
                1 - after_checkpoint,
                "ExactRecovery",
            ),
            (&recovered, "once", 0, "Idempotence"),
            (&recovered, "entered", 2, "NoReplay"),
            (&superseded, "entered", 1, "NewGrantProgress"),
            (&superseded, "kept", 0, "PreserveEvidence"),
        ] {
            let mut bad = state.clone();
            bad.insert(field, value);
            assert!(!model.check_invariant(law, &bad));
        }
    }
}
