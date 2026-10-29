// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

fn profile_bytes(kind: i64) -> &'static [u8] {
    match kind {
        1 => b"{}",
        2 => br#"{"oauthAccount":{"accountUuid":"ordinary-uuid"}}"#,
        3 => br#"{"oauthAccount":{"accountUuid":"reserved-uuid"}}"#,
        _ => unreachable!(),
    }
}

fn records(store: &accounts::Store, roster: &Path) -> Vec<Option<Vec<u8>>> {
    [
        roster.to_path_buf(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
        store.state_witness_file(),
        store.logins_witness_file(),
        store.dir.join("sync-commit.json"),
    ]
    .map(|path| std::fs::read(path).ok())
    .into_iter()
    .collect()
}

struct Host {
    store: accounts::Store,
    roster: PathBuf,
    root: PathBuf,
    seen: i64,
    kind: i64,
    legacy: bool,
    now: i64,
    home: RefCell<Option<PathBuf>>,
    before: RefCell<Option<Vec<Option<Vec<u8>>>>>,
    calls: Cell<i64>,
    guarded: Cell<bool>,
}

impl Host {
    fn profile(&self, home: &Path) -> PathBuf {
        home.join(if self.legacy {
            ".config.json"
        } else {
            ".claude.json"
        })
    }
}

impl AccountsHost for Host {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        self.now
    }

    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        let home = PathBuf::from(dir.unwrap());
        std::fs::write(self.profile(&home), profile_bytes(self.seen)).unwrap();
        *self.home.borrow_mut() = Some(home);
        Ok(0)
    }

    fn auth_status(&self, home: &str) -> Result<Captured, String> {
        let path = self.profile(Path::new(home));
        assert_eq!(std::fs::read(&path).unwrap(), profile_bytes(self.seen));
        let answer = said(true);
        if self.kind == 0 {
            std::fs::rename(path, self.root.join("retained-profile.json")).unwrap();
        } else if self.kind != self.seen {
            std::fs::write(path, profile_bytes(self.kind)).unwrap();
        }
        *self.before.borrow_mut() = Some(records(&self.store, &self.roster));
        answer
    }

    fn claude_principal_in_home(&self, home: &str) -> Option<[u8; 32]> {
        self.calls.set(self.calls.get() + 1);
        let observed = profile::principal_in_home(Path::new(home));
        // The account writer must be held here: a free writer marks an observation
        // hoisted before it.
        let attempt = self
            .store
            .transact_within(&[], self.now, &mut None, |_| Ok(Vec::new()));
        self.guarded.set(
            attempt
                .as_ref()
                .is_err_and(|why| why == accounts::LOCK_HELD),
        );
        observed
    }
}

fn case(custody: i64, seen: i64, kind: i64, prior: bool, legacy: bool) -> [(&'static str, i64); 7] {
    let tmp = tmp_dir("plain-login-commit");
    let env = env_at(&tmp);
    assert_eq!(acc_quiet(add("work", Class::Dedicated), &env).0, exit(0));
    let store = env.accounts_store();
    let old = tmp.path().join("previous");
    let previous = if prior {
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join(".claude.json"), profile_bytes(2)).unwrap();
        std::fs::write(old.join("owner-note"), "retain").unwrap();
        store
            .transact_logins(env.now, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    old.to_str().unwrap(),
                    None,
                    env.now,
                ))
            })
            .unwrap();
        Some(every_file(&old))
    } else {
        None
    };
    if custody == 1 {
        let reserved = store.claim_staging_dir("reserved").unwrap();
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "reserved".into(),
                        identity: "reserved@example.test".into(),
                        staging: reserved.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: env.now,
                    },
                    profile::principal_id("reserved-uuid"),
                )?;
                Ok(Vec::new())
            })
            .unwrap();
    }
    let before_logins = store.load_logins(env.now).unwrap().0;
    let roster = env.accounts_roster().unwrap();
    let host = Host {
        store: store.clone(),
        roster: roster.clone(),
        root: tmp.path().into(),
        seen,
        kind,
        legacy,
        now: env.now + 1,
        home: RefCell::new(None),
        before: RefCell::new(None),
        calls: Cell::new(0),
        guarded: Cell::new(true),
    };
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "work".into(),
        },
        &env,
        &host,
    );
    let published = code == exit(0);
    let home = host.home.borrow().clone().unwrap();
    let after_logins = store.load_logins(host.now).unwrap().0;
    if !published {
        assert_eq!(after_logins, before_logins, "{err}");
    }
    let path = if kind == 0 {
        host.root.join("retained-profile.json")
    } else {
        host.profile(&home)
    };
    let expected = profile_bytes(if kind == 0 { seen } else { kind });
    let preserved = std::fs::read(path).unwrap() == expected
        && previous.is_none_or(|files| every_file(&old) == files);
    assert!(!home.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
    [
        ("phase", 2),
        ("published", i64::from(published)),
        (
            "bound",
            i64::from(
                after_logins
                    .get("work")
                    .and_then(|binding| binding.dir.as_deref())
                    == home.to_str(),
            ),
        ),
        (
            "unchanged",
            i64::from(host.before.borrow().as_ref().unwrap() == &records(&store, &roster)),
        ),
        ("preserved", i64::from(preserved)),
        ("guarded", i64::from(host.guarded.get())),
        ("calls", host.calls.get()),
    ]
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action != "Commit" {
        account_plain_login_commit_environment();
    }
    match action {
        "Custody" => {
            next.insert("custody", 1);
        }
        "Credential" | "ChangeCredential" => {
            next.insert("credential", state["credential"] + 1);
        }
        "LoseCredential" => {
            next.insert("credential", 0);
        }
        "Prior" => {
            next.insert("prior", 1);
        }
        "Report" => {
            next.insert("phase", 1);
            next.insert("seen", state["credential"]);
        }
        "Commit" => {
            let observed = case(
                state["custody"],
                state["seen"],
                state["credential"],
                state["prior"] == 1,
                false,
            );
            assert_eq!(
                observed,
                case(
                    state["custody"],
                    state["seen"],
                    state["credential"],
                    state["prior"] == 1,
                    true,
                )
            );
            for (field, value) in observed {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn plain_login_commit_conforms_and_observes_under_the_writer() {
    let model = aterm_models_harness::plain_login_commit_model();
    assert_eq!(model.mutant_dials(), (1..=10).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 108);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for kind in 0..=3 {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("seen", 2);
        state.insert("credential", kind);
        state.insert("custody", 1);
        let good = transition(&state, "Commit");
        for (field, value, law) in [
            ("published", 1 - good["published"], "Publication"),
            ("bound", 1 - good["bound"], "Binding"),
            ("unchanged", 1 - good["unchanged"], "Records"),
            ("preserved", 0, "Evidence"),
            ("calls", good["calls"] + 1, "OneRead"),
        ] {
            let mut bad = good.clone();
            bad.insert(field, value);
            assert!(!model.check_invariant(law, &bad), "{law}");
        }
        if good["calls"] > 0 {
            let mut bad = good.clone();
            bad.insert("guarded", 0);
            assert!(!model.check_invariant("LockedObservation", &bad));
        }
    }
}
