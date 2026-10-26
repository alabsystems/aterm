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
    fn claude_principal_in_home(&self, home: &str) -> Option<[u8; 32]> {
        profile::principal_in_home(Path::new(home))
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
        self.base.status_in_home(home)
    }
    fn claude_principal_in_home(&self, home: &str) -> Option<[u8; 32]> {
        if self.case == 8 {
            None
        } else {
            self.base.claude_principal_in_home(home)
        }
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
