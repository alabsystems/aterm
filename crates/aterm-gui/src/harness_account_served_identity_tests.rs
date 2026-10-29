// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Clone, Copy)]
struct Case {
    named: bool,
    before: i64,
    credential: i64,
    before_auth: i64,
    auth: i64,
    method: i64,
    legacy: bool,
    dedicated: bool,
}

fn auth(value: i64) -> accounts::Auth {
    match value {
        0 => accounts::Auth::Unseen,
        1 => accounts::Auth::Ok,
        2 => accounts::Auth::SignedOut,
        _ => unreachable!(),
    }
}

fn set_auth(a: &Accts, value: i64) {
    edited(a, |state| {
        let row = state.accounts.entry("main".into()).or_default();
        row.auth = auth(value);
        row.auth_at = (value > 0).then_some(a.clock());
    });
}

fn profile_bytes(evidence: i64) -> Vec<u8> {
    let data = match evidence {
        0 => r#"{"oauthAccount":{"accountUuid":"unknown"}}"#,
        1 => r#"{"oauthAccount":{"emailAddress":"owner@example.test"}}"#,
        2 => r#"{"oauthAccount":{"emailAddress":"wrong@example.test"}}"#,
        _ => unreachable!(),
    };
    data.as_bytes().to_vec()
}

fn images(a: &Accts) -> [Option<Vec<u8>>; 4] {
    let store = a.store();
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|file| std::fs::read(file).ok())
}

struct Trial {
    a: Arc<Accts>,
    idle: WorkerIdle,
    profile: PathBuf,
    other_profile: Option<PathBuf>,
}

impl Trial {
    fn new(case: Case) -> Self {
        let a = Accts::new();
        let row = accounts::Row {
            label: "main".into(),
            class: accounts::Class::Dedicated,
            email: case.named.then(|| "alias@example.test".into()),
            identity_email: case.named.then(|| "OWNER@EXAMPLE.TEST".into()),
            dir: case.dedicated.then(|| a.login_dir("work-2")),
            ..accounts::Row::default()
        };
        std::fs::write(
            a.roster_path(),
            accounts::append_rows_for(accounts::Cli::Claude, "", &[row]).unwrap(),
        )
        .unwrap();
        let sid = "s-served-identity-model";
        if case.dedicated {
            a.exec(
                sid,
                ACCT_TALK_A,
                Some(&a.login_dir("work-2")),
                ACCT_NOW - 100,
            );
        } else {
            a.spawn(sid, ACCT_TALK_A, "main");
        }
        let idle = a.worker(sid, &on());
        let seat = (idle.hooks.acts.account_seat)().unwrap();
        edited(&a, |state| {
            state.active = Some("main".into());
            state.proven = false;
            state.canary = Some(accounts::Canary {
                sid: sid.into(),
                at: a.clock(),
                took: false,
            });
        });
        set_auth(&a, case.before_auth);
        {
            let mut kept = idle.kept.lock().unwrap();
            let process = kept.snapshot.clone().unwrap();
            kept.account.moved = Some(Moved {
                id: MoveId::default(),
                origin: seat.context(),
                to: "main".into(),
                at: AccountAt::Wall,
                login: None,
                process,
                untold: false,
                answered: false,
            });
        }
        let profile = if case.dedicated {
            PathBuf::from(a.login_dir("work-2")).join(if case.legacy {
                ".config.json"
            } else {
                ".claude.json"
            })
        } else {
            a.tmp.path().join(if case.legacy {
                "home/.claude/.config.json"
            } else {
                "home/.claude.json"
            })
        };
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, profile_bytes(case.before)).unwrap();
        let other_profile = case
            .dedicated
            .then(|| a.tmp.path().join("home/.claude.json"));
        if let Some(other) = &other_profile {
            std::fs::write(other, profile_bytes(if case.before == 1 { 2 } else { 1 })).unwrap();
        }
        Self {
            a,
            idle,
            profile,
            other_profile,
        }
    }

    fn call(&self, method: i64) {
        match method {
            0 => self.idle.account_served(),
            1 => {
                let (seat, roster, look) = self.idle.account_ctx().unwrap();
                self.idle.signed_back(&seat, &roster, &look, "main");
            }
            _ => unreachable!(),
        }
    }

    fn first(&self, case: Case) -> (bool, bool) {
        let at_writer = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&at_writer);
        let path = self.profile.clone();
        let expected = profile_bytes(case.credential);
        let written = expected.clone();
        let other = self.other_profile.clone();
        let other_expected = profile_bytes(if case.credential == 1 { 2 } else { 1 });
        let other_written = other_expected.clone();
        self.a.at_seat_read(2, move |a| {
            set_auth(a, case.auth);
            std::fs::write(path, written).unwrap();
            if let Some(other) = other {
                std::fs::write(other, other_written).unwrap();
            }
            *recorded.lock().unwrap() = Some(images(a));
        });
        self.call(case.method);
        let before = at_writer.lock().unwrap().take().expect("writer reached");
        (
            images(&self.a) == before,
            std::fs::read(&self.profile).unwrap() == expected
                && self
                    .other_profile
                    .as_ref()
                    .is_none_or(|path| std::fs::read(path).unwrap() == other_expected),
        )
    }

    fn again(&self, method: i64) -> (bool, bool) {
        let before = images(&self.a);
        let profile = std::fs::read(&self.profile).unwrap();
        let other = self
            .other_profile
            .as_ref()
            .map(|path| std::fs::read(path).unwrap());
        self.call(method);
        (
            images(&self.a) == before,
            std::fs::read(&self.profile).unwrap() == profile
                && self
                    .other_profile
                    .as_ref()
                    .map(|path| std::fs::read(path).unwrap())
                    == other,
        )
    }
}

fn run(case: Case, poll: bool, retry: bool, replay: bool) -> [(&'static str, i64); 8] {
    let trial = Trial::new(case);
    let (mut quiet, mut preserved) = trial.first(case);
    let mut writes = i64::from(!quiet);
    if poll {
        let next = trial.again(case.method);
        quiet = next.0;
        writes += i64::from(!quiet);
        preserved &= next.1;
    }
    if retry {
        std::fs::write(&trial.profile, profile_bytes(1)).unwrap();
        if let Some(other) = &trial.other_profile {
            std::fs::write(other, profile_bytes(2)).unwrap();
        }
        let next = trial.again(case.method);
        quiet = next.0;
        writes += i64::from(!quiet);
        preserved &= next.1;
    }
    if replay {
        let next = trial.again(case.method);
        quiet = next.0;
        writes += i64::from(!quiet);
        preserved &= next.1;
    }
    let state = trial.a.state();
    [
        ("writes", writes),
        ("proven", i64::from(state.proven)),
        (
            "recovered",
            i64::try_from(
                trial
                    .a
                    .ledger()
                    .iter()
                    .filter(|kind| **kind == "auth")
                    .count(),
            )
            .unwrap(),
        ),
        (
            "auth_result",
            match state.account("main").auth {
                accounts::Auth::Unseen => 0,
                accounts::Auth::Ok => 1,
                accounts::Auth::SignedOut => 2,
            },
        ),
        (
            "remaining",
            i64::from(trial.idle.kept.lock().unwrap().account.moved.is_some()),
        ),
        ("canary", i64::from(state.canary.is_some())),
        ("preserved", i64::from(preserved)),
        ("quiet", i64::from(quiet)),
    ]
}

fn case(state: &aterm_spec::interp::State, legacy: bool, dedicated: bool) -> Case {
    Case {
        named: state["named"] == 1,
        before: state["before"],
        credential: if state["phase"] >= 2 {
            state["denied"]
        } else {
            state["credential"]
        },
        before_auth: state["before_auth"],
        auth: state["auth"],
        method: state["method"],
        legacy,
        dedicated,
    }
}

fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
    legacy: bool,
    dedicated: bool,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Named" => {
            next.insert("named", 1);
        }
        "Before" | "Credential" | "BeforeAuth" | "Auth" => {
            let field = match action {
                "Before" => "before",
                "Credential" => "credential",
                "BeforeAuth" => "before_auth",
                _ => "auth",
            };
            next.insert(field, state[field] + 1);
            if action == "Auth" {
                next.insert("auth_result", state["auth"] + 1);
            }
        }
        "Method" => {
            next.insert("method", 1);
        }
        "Correct" => {
            next.insert("credential", 1);
            next.insert("phase", 2);
        }
        "Observe" | "Poll" | "Retry" | "Replay" => {
            let poll = action == "Poll" || state["polls"] == 1;
            let retry = action == "Retry" || (action == "Replay" && state["phase"] == 3);
            for (key, value) in run(
                case(state, legacy, dedicated),
                poll,
                retry,
                action == "Replay",
            ) {
                next.insert(key, value);
            }
            match action {
                "Observe" => {
                    next.insert("phase", 1);
                    next.insert("denied", state["credential"]);
                }
                "Poll" => {
                    next.insert("polls", 1);
                }
                "Retry" => {
                    next.insert("phase", 3);
                }
                "Replay" => {
                    next.insert("phase", 4);
                }
                _ => unreachable!(),
            }
        }
        _ => unreachable!(),
    }
    next
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition(state, action, false, false)
}

#[test]
fn served_identity_conforms_at_every_writer_and_replay() {
    let model = aterm_models_harness::account_served_identity_model();
    assert_eq!(model.mutant_dials(), (1..=19).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 1188);
    // Each step builds its own accounts world, little but file system work, so the steps run on
    // the file system lane's workers ([`aterm_spec::fs_lane::PLACES`]).
    let mut steps = Vec::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                for (legacy, dedicated) in
                    [(false, false), (false, true), (true, false), (true, true)]
                {
                    steps.push((state.clone(), action.name, legacy, dedicated));
                }
            }
        }
    }
    aterm_spec::check::each_on(
        aterm_spec::fs_lane::PLACES,
        &steps,
        |&(ref state, action, legacy, dedicated)| {
            let next = transition(state, action, legacy, dedicated);
            assert!(
                model.successors(action, state).contains(&next),
                "{action} legacy={legacy} dedicated={dedicated}: {state:?} -> {next:?}"
            );
        },
    );
    let mut base = model.init_state();
    base.insert("named", 1);
    base.insert("credential", 2);
    for (field, value, law) in [
        ("writes", 1, "Admission"),
        ("proven", 1, "Proven"),
        ("recovered", 1, "Recovery"),
        ("auth_result", 1, "Authentication"),
        ("remaining", 0, "Move"),
        ("canary", 0, "Canary"),
        ("preserved", 0, "Evidence"),
        ("quiet", 0, "Quiet"),
    ] {
        let mut bad = identity_transition(&base, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    let mut good = base;
    good.insert("credential", 1);
    let mut bad = identity_transition(&good, "Observe");
    bad.insert("phase", 3);
    bad.insert("writes", 0);
    assert!(!model.check_invariant("Progress", &bad));
    bad.insert("writes", 2);
    assert!(!model.check_invariant("Once", &bad));
}

#[test]
fn identity_uses_the_profile_selectors_of_one_process_observation() {
    for method in [0, 1] {
        for custom in [false, true] {
            let mut trial = Trial::new(Case {
                named: true,
                before: 2,
                credential: 1,
                before_auth: 2,
                auth: 2,
                method,
                legacy: false,
                dedicated: false,
            });
            let selected_home = trial.a.tmp.path().join("observed-home");
            std::fs::create_dir_all(&selected_home).unwrap();
            let selected = selected_home.join(if custom {
                ".claude-custom-oauth.json"
            } else {
                ".claude.json"
            });
            std::fs::write(&selected, profile_bytes(1)).unwrap();
            if custom {
                std::fs::write(selected_home.join(".claude.json"), profile_bytes(2)).unwrap();
            }
            let first = vec![
                "PATH=/usr/bin".to_string(),
                format!("{}={}", accounts::HOMEDIR_ENV, selected_home.display()),
                format!(
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL={}",
                    if custom {
                        "https://example.invalid/oauth"
                    } else {
                        ""
                    }
                ),
            ];
            let later = vec![
                "PATH=/usr/bin".to_string(),
                format!(
                    "{}={}",
                    accounts::HOMEDIR_ENV,
                    trial.a.tmp.path().join("home").display()
                ),
            ];
            let reads = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&reads);
            trial.idle.hooks.acts.account_env = Arc::new(move |_| {
                Some(if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    first.clone()
                } else {
                    later.clone()
                })
            });
            trial.call(method);
            assert_eq!(
                reads.load(Ordering::SeqCst),
                1,
                "method={method} custom={custom}"
            );
            assert_eq!(trial.a.state().account("main").auth, accounts::Auth::Ok);
            assert_eq!(trial.a.state().proven, method == 0);
            assert_eq!(
                trial.idle.kept.lock().unwrap().account.moved.is_some(),
                method == 1
            );
            assert_eq!(std::fs::read(&selected).unwrap(), profile_bytes(1));
            assert_eq!(std::fs::read(&trial.profile).unwrap(), profile_bytes(2));
        }
    }
}
