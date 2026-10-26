// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

type Records = [Option<Vec<u8>>; 7];

struct Bed {
    _root: Tmp,
    env: Env,
    cli: Cli,
    path: PathBuf,
    store: accounts::Store,
    raw: Roster,
    credential: PathBuf,
    profile: Option<PathBuf>,
}

impl Bed {
    fn new(cli: Cli, browser: bool) -> Self {
        Self::with_binding(cli, browser, true)
    }

    fn with_binding(cli: Cli, browser: bool, bound: bool) -> Self {
        let root = tmp_dir("signin-epoch");
        let env = env_at(&root);
        let path = env.accounts_roster_for(cli).unwrap();
        let store = env.accounts_store_for(cli).unwrap();
        let dir = root.path().join("old");
        let credential = dir.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(&credential, b"existing credential").unwrap();
        let raw = Roster {
            rows: vec![accounts::Row {
                label: "main".into(),
                dir: Some(dir.to_str().unwrap().into()),
                email: browser.then(|| "main@example.test".into()),
                vendor: accounts::Vendor {
                    region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        std::fs::write(
            &path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        if bound {
            store
                .transact_logins(env.now, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        None,
                        dir.to_str().unwrap(),
                        None,
                        env.now,
                    ))
                })
                .unwrap();
        }
        let profile = browser.then(|| {
            crate::harness::headless_oauth::row_browser_profile(
                cli,
                &store.dir,
                &store.shared_dir(),
                "main",
                Some("main@example.test"),
            )
        });
        Self {
            _root: root,
            env,
            cli,
            path,
            store,
            raw,
            credential,
            profile,
        }
    }

    fn records(&self) -> Records {
        [
            self.path.clone(),
            self.store.state_file(),
            self.store.logins_file(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
            self.store.ledger_path(),
            self.store.dir.join("sync-commit.json"),
        ]
        .map(|path| std::fs::read(path).ok())
    }

    fn change(&self, case: i64) {
        match case {
            1 | 4 => {
                let mut raw = self.raw.clone();
                raw.rows[0].class = accounts::Class::Shared;
                std::fs::write(
                    &self.path,
                    accounts::append_rows_for(self.cli, "", &raw.rows).unwrap(),
                )
                .unwrap();
            }
            2 | 5 | 9 | 10 => {
                let label = if case < 9 { "main" } else { "other" };
                let dir = if label == "main" {
                    self.raw.rows[0].dir.clone().unwrap()
                } else {
                    self.store
                        .login_dir(label)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned()
                };
                self.store
                    .transact_logins(self.env.now, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            label,
                            None,
                            &dir,
                            None,
                            self.env.now,
                        ))
                    })
                    .unwrap();
            }
            3 | 6 => std::fs::write(self.store.state_file(), "{").unwrap(),
            7 | 8 => {
                self.store
                    .transact(self.env.now, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            "other",
                            "weekly",
                            Some(self.env.now + 3600),
                            self.env.now,
                            self.env.now,
                        ))
                    })
                    .unwrap();
            }
            _ => unreachable!(),
        }
    }
}

struct Host<'a> {
    bed: &'a Bed,
    case: i64,
    browser: bool,
    changed: Cell<bool>,
    after: RefCell<Records>,
    opened: Cell<usize>,
    tidied: Cell<usize>,
}

impl Host<'_> {
    fn before(&self) {
        if matches!(self.case, 1..=3 | 7 | 9) {
            self.change();
        }
    }
    fn during(&self) {
        assert!(
            self.bed.store.signin_now().unwrap().is_none(),
            "authentication holds the machine sign-in lock"
        );
        self.opened.set(self.opened.get() + 1);
        if matches!(self.case, 4..=6 | 8 | 10) {
            self.change();
        }
    }
    fn change(&self) {
        assert!(!self.changed.replace(true));
        self.bed.change(self.case);
        *self.after.borrow_mut() = self.bed.records();
    }
}

impl AccountsHost for Host<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        said(true)
    }
    fn unaskable(&self) -> Option<String> {
        assert!(!self.browser);
        self.before();
        None
    }
    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        self.during();
        std::fs::write(Path::new(dir.unwrap()).join(".claude.json"), "{}").unwrap();
        Ok(0)
    }
    fn now(&self) -> i64 {
        if self.case == 11 {
            self.bed.env.now - 60
        } else {
            self.bed.env.now + 1
        }
    }
    fn window_browser(&self) -> Option<PathBuf> {
        assert!(self.browser);
        self.before();
        Some(PathBuf::from("/fixture/Chromium"))
    }
    fn open_window(
        &self,
        _: &Path,
        profile: &Path,
        _: &str,
    ) -> Result<crate::harness::browser_window::WindowEnd, String> {
        assert_eq!(Some(profile), self.bed.profile.as_deref());
        self.during();
        Ok(crate::harness::browser_window::WindowEnd::Done)
    }
    fn tidy_profile(
        &self,
        _: &Path,
        profile: &Path,
        _: Option<(&str, &str)>,
    ) -> Result<crate::harness::browser_window::Tidied, String> {
        assert_eq!(Some(profile), self.bed.profile.as_deref());
        assert!(
            self.bed.store.signin_now().unwrap().is_none(),
            "cleanup holds the machine sign-in lock"
        );
        self.tidied.set(self.tidied.get() + 1);
        Ok(crate::harness::browser_window::Tidied {
            dropped: 0,
            session: Some(true),
        })
    }
}

fn exercise(browser: bool, provider: i64, case: i64) -> (bool, bool, bool, i64) {
    let cli =
        [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok][usize::try_from(provider - 1).unwrap()];
    let bed = Bed::new(cli, browser);
    let host = Host {
        bed: &bed,
        case,
        browser,
        changed: Cell::new(false),
        after: RefCell::new(bed.records()),
        opened: Cell::new(0),
        tidied: Cell::new(0),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = if browser {
        run_accounts_browser_with(cli, &bed.env, &host, "main", &mut out, &mut err, None)
    } else {
        run_accounts_login(&bed.env, &host, "main", &mut out, &mut err)
    };
    let before = host.after.borrow();
    let after = bed.records();
    let recorded = before[5] != after[5];
    let mut event_order = 1;
    if !recorded {
        assert_eq!(
            after, *before,
            "a refused result must preserve the current account records"
        );
        assert_eq!(status, ExitCode::FAILURE);
    } else {
        assert_eq!(status, ExitCode::SUCCESS);
        let kind = if browser {
            accounts::Kind::SignIn
        } else {
            accounts::Kind::Renewed
        };
        let events = bed.store.tail(100).unwrap();
        let completion = events
            .iter()
            .rev()
            .find(|event| event.kind == kind && event.account == "main")
            .unwrap();
        event_order = 1 + (completion.t - bed.env.now).signum();
        assert_eq!(
            bed.store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == kind && event.account == "main")
                .count(),
            if browser { 1 } else { 2 }
        );
        if matches!(case, 7 | 8) {
            let expected = accounts::State::from_json(
                std::str::from_utf8(before[1].as_ref().unwrap()).unwrap(),
                bed.env.now,
            )
            .unwrap();
            assert_eq!(
                bed.store.load(bed.env.now).unwrap().0.accounts.get("other"),
                expected.accounts.get("other"),
            );
        }
        if matches!(case, 9 | 10) {
            let expected = accounts::Logins::from_json(
                std::str::from_utf8(before[2].as_ref().unwrap()).unwrap(),
                bed.env.now,
            )
            .unwrap();
            assert!(expected.get("other").is_some());
            assert_eq!(
                bed.store.load_logins(bed.env.now).unwrap().0.get("other"),
                expected.get("other"),
            );
        }
        if browser && case == 11 {
            assert_eq!(
                &after[1..3],
                &before[1..3],
                "a browser clock rollback must preserve existing account and login records"
            );
        }
    }
    assert_eq!(
        std::fs::read(&bed.credential).unwrap(),
        b"existing credential"
    );
    assert!(host.opened.get() <= 1);
    assert!(host.tidied.get() <= 1);
    if host.opened.get() == 0 {
        assert!(!bed.store.logins_dir().join("main+1").exists());
        assert!(bed.profile.as_ref().is_none_or(|path| !path.exists()));
    }
    (
        host.opened.get() == 1,
        recorded,
        host.tidied.get() == 1,
        event_order,
    )
}

pub(crate) fn epoch_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Browser" => {
            next.insert("browser", 1);
        }
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Run" => {
            let (opened, recorded, tidied, event_order) =
                exercise(state["browser"] == 1, state["provider"], state["case"]);
            next.insert("phase", 1);
            next.insert("opened", i64::from(opened));
            next.insert("recorded", i64::from(recorded));
            next.insert("tidied", i64::from(tidied));
            next.insert("event_order", event_order);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn signin_and_browser_results_refine_the_captured_account_epoch() {
    let model = aterm_models_harness::account_signin_epoch_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5, 6, 7]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = epoch_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Run");
            }
        }
    }
    assert_eq!(cases, 60);
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    bad.insert("case", 1);
    bad.insert("opened", 1);
    assert!(!model.check_invariant("StaleBeforeOpenRefused", &bad));
    bad.insert("case", 0);
    bad.insert("opened", 0);
    assert!(!model.check_invariant("CurrentBeforeOpenAdmitted", &bad));
    bad.insert("case", 4);
    bad.insert("recorded", 1);
    assert!(!model.check_invariant("StaleResultNotRecorded", &bad));
    bad.insert("case", 0);
    bad.insert("recorded", 0);
    assert!(!model.check_invariant("CurrentResultRecorded", &bad));
    bad.insert("browser", 1);
    bad.insert("opened", 1);
    assert!(!model.check_invariant("OpenedBrowserTidied", &bad));
    bad.insert("case", 11);
    bad.insert("recorded", 1);
    bad.insert("event_order", 0);
    assert!(!model.check_invariant("ResultNotBeforeStart", &bad));
    bad.insert("case", 0);
    bad.insert("event_order", 1);
    assert!(!model.check_invariant("LaterObservationKept", &bad));
}

#[test]
fn unbound_rows_keep_the_conservative_global_counter_guard() {
    for browser in [false, true] {
        for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
            if !browser && cli != Cli::Claude {
                continue;
            }
            for case in [0, 9] {
                let bed = Bed::with_binding(cli, browser, false);
                let host = Host {
                    bed: &bed,
                    case,
                    browser,
                    changed: Cell::new(false),
                    after: RefCell::new(bed.records()),
                    opened: Cell::new(0),
                    tidied: Cell::new(0),
                };
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let status = if browser {
                    run_accounts_browser_with(
                        cli, &bed.env, &host, "main", &mut out, &mut err, None,
                    )
                } else {
                    run_accounts_login(&bed.env, &host, "main", &mut out, &mut err)
                };
                assert_eq!(
                    host.opened.get(),
                    usize::from(case == 0),
                    "{cli:?}: {}",
                    String::from_utf8_lossy(&err)
                );
                assert_eq!(
                    status,
                    if case == 0 {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                );
                if case > 0 {
                    assert_eq!(bed.records(), *host.after.borrow());
                }
            }
        }
    }
}
