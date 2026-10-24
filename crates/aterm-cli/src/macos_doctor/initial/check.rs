// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only observations of the displayed setup checklist.

use super::{
    Action, InitialHost, InitialSetupObserved, LiveInitial, NativeHost, Path, Plan, Report, Row,
    Selection, Stage, access_revision_at, accounts, ensure_person_scope, execute, item_rows,
    observe_read_only, preferences, readiness_row, rows_ready, state_root,
};
use aterm_secrets::client::{self, Membership, Paths, Status};
use aterm_secrets::service::{self, Job, State};

trait InitialCheckHost {
    fn workstation(&mut self, root: &Path, selected: Selection) -> Result<Vec<Row>, String>;
    fn accounts(&mut self, root: &Path, selected: Selection) -> Result<Vec<Row>, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String>;
    fn vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    fn repositories(&mut self) -> Result<Vec<Row>, String>;
}

#[derive(Default)]
struct LiveCheck {
    workstation: Option<Result<Vec<Row>, String>>,
    accounts: Option<Result<Vec<Row>, String>>,
    spotlight: Option<Result<Vec<Row>, String>>,
    vault: Option<Result<Vec<Row>, String>>,
}

impl LiveCheck {
    /// Read the plan's slow stages at once, each on its own thread: the Mac settings (then the
    /// sign-ins, one pass after the other), Spotlight and the vault. Aterm's permissions are
    /// read from the access record when the check reaches them.
    fn prefetch(root: &Path, plan: &Plan) -> Self {
        let check = |selected: Selection| {
            execute(&mut NativeHost::selected(selected), root, Action::Check)
                .map(|report| report.rows)
        };
        let (prefs, signins) = (preferences(plan.selection), accounts(plan.selection));
        let ws_selected = Stage::Workstation.selected(plan);
        let accounts_selected = Stage::Accounts.selected(plan);
        std::thread::scope(|s| {
            let ws = (ws_selected || accounts_selected).then(|| {
                s.spawn(move || {
                    (
                        ws_selected.then(|| check(prefs)),
                        accounts_selected.then(|| check(signins)),
                    )
                })
            });
            let sp = Stage::Spotlight.selected(plan).then(|| {
                s.spawn(|| {
                    aterm_machines::init::run_initial_spotlight(
                        &aterm_machines::port::Live::new(),
                        &LiveInitial::live().context()?,
                        aterm_machines::model::Mode::Plan,
                    )
                    .map(item_rows)
                })
            });
            let vault = Stage::Secrets
                .selected(plan)
                .then(|| s.spawn(|| current_vault_rows(plan)));
            let (workstation, accounts) = ws
                .map(|h| h.join().expect("workstation check thread"))
                .unwrap_or_default();
            Self {
                workstation,
                accounts,
                spotlight: sp.map(|h| h.join().expect("spotlight check thread")),
                vault: vault.map(|h| h.join().expect("vault check thread")),
            }
        })
    }
}

impl InitialCheckHost for LiveCheck {
    fn workstation(&mut self, root: &Path, selected: Selection) -> Result<Vec<Row>, String> {
        self.workstation.take().unwrap_or_else(|| {
            execute(&mut NativeHost::selected(selected), root, Action::Check)
                .map(|report| report.rows)
        })
    }

    fn accounts(&mut self, root: &Path, selected: Selection) -> Result<Vec<Row>, String> {
        self.accounts.take().unwrap_or_else(|| {
            execute(&mut NativeHost::selected(selected), root, Action::Check)
                .map(|report| report.rows)
        })
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        self.spotlight.take().unwrap_or_else(|| {
            aterm_machines::init::run_initial_spotlight(
                &aterm_machines::port::Live::new(),
                &LiveInitial::live().context()?,
                aterm_machines::model::Mode::Plan,
            )
            .map(item_rows)
        })
    }

    fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String> {
        LiveInitial::live().permissions(plan, revision)
    }

    fn vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        self.vault
            .take()
            .unwrap_or_else(|| current_vault_rows(plan))
    }

    fn repositories(&mut self) -> Result<Vec<Row>, String> {
        aterm_machines::init_github::run_initial_repositories(
            &aterm_machines::port::Live::new(),
            &LiveInitial::live().context()?,
            aterm_machines::model::Mode::Plan,
        )
        .map(item_rows)
    }
}

/// Reads local vault readiness for an explicit check or before repository access.
pub(super) fn current_vault_rows(plan: &Plan) -> Result<Vec<Row>, String> {
    let context = LiveInitial::live().context()?;
    let [user] = context.users.as_slice() else {
        return Err("checking the vault needs exactly one console account".into());
    };
    let paths = Paths::for_home(Path::new(&user.home));
    let status = client::status(&paths).map_err(|error| error.to_string())?;
    vault_rows(
        plan,
        &paths.home,
        &status,
        |expected| {
            client::materialization_ready_for(&paths, expected).map_err(|error| error.to_string())
        },
        || {
            let running = service::state(&paths.home, Job::Subscriber)
                .map(|state| matches!(state, State::Installed { pid: Some(_), .. }))
                .map_err(|error| error.to_string())?;
            Ok(if running {
                Subscribed::Running
            } else if service::subscriber_turned_off(&paths.home) {
                Subscribed::TurnedOff
            } else {
                Subscribed::Down
            })
        },
    )
}

/// This login's subscriber, as the vault's readiness reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Subscribed {
    /// It runs.
    Running,
    /// It does not run, and the person turned it off
    /// (`aterm_secrets::service::subscriber_off_path`): their choice, which the
    /// vault's readiness honors with the files as last synced.
    TurnedOff,
    /// It does not run.
    Down,
}

pub(super) fn selected_vault<'a>(
    plan: &Plan,
    home: &Path,
    status: &'a Status,
) -> Result<Option<&'a client::Config>, String> {
    aterm_machines::init_secrets::selected_vault(&plan.vault_url, home, status.config.as_ref())
}

/// Builds the vault membership and subscriber rows of `status` for `plan`.
pub(super) fn vault_rows(
    plan: &Plan,
    home: &Path,
    status: &Status,
    materialized: impl FnOnce(&client::Config) -> Result<bool, String>,
    subscriber: impl FnOnce() -> Result<Subscribed, String>,
) -> Result<Vec<Row>, String> {
    let Some(config) = selected_vault(plan, home, status)? else {
        return Ok(vec![Row {
            name: Stage::Secrets.name().into(),
            status: "drift".into(),
            detail: "vault source or signer differs; Set up now to enroll".into(),
        }]);
    };
    let membership_ready = matches!(status.membership, Membership::In { .. } | Membership::Plain);
    let ready = membership_ready && materialized(config)?;
    let detail = match &status.membership {
        Membership::In { seq, .. } if ready => {
            format!("synced files verified at version {seq}")
        }
        Membership::Plain if ready => "synced files verified".into(),
        Membership::In { .. } | Membership::Plain => {
            "vault files are incomplete; Set up now to retry".into()
        }
        Membership::Pending => match &status.machine {
            Some((_, code)) => format!("waiting for vault approval, code {code}"),
            None => "no machine identity is available; Set up now to enroll".into(),
        },
        Membership::Revoked { .. } => "this machine no longer has vault membership".into(),
        Membership::Unconfigured => "no vault membership is configured".into(),
    };
    let subscriber = subscriber()?;
    Ok(vec![
        readiness_row(Stage::Secrets.name(), ready, detail),
        readiness_row(
            aterm_types::macos_setup::SECRETS_SUBSCRIBER_ROW,
            subscriber != Subscribed::Down,
            match subscriber {
                Subscribed::Running => String::new(),
                Subscribed::TurnedOff => "turned off on this Mac by choice".into(),
                Subscribed::Down => {
                    "subscriber is not running; Set up now to install or restart it".into()
                }
            },
        ),
    ])
}

fn check_with(host: &mut dyn InitialCheckHost, root: &Path, plan: &Plan) -> Report {
    let mut report = Report {
        action: Action::Check,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
    };
    let mut vault_ready = !Stage::Secrets.selected(plan);
    for stage in Stage::ALL {
        if !stage.selected(plan) {
            continue;
        }
        if stage == Stage::Repositories && !vault_ready {
            report.rows.push(readiness_row(
                stage.name(),
                false,
                "waiting for the selected vault to synchronize credentials".into(),
            ));
            continue;
        }
        let result = match stage {
            Stage::Workstation => host.workstation(root, preferences(plan.selection)),
            Stage::Spotlight => host.spotlight(),
            Stage::Permissions => host.permissions(plan, access_revision_at(root).unwrap_or(0)),
            Stage::Secrets => host.vault(plan),
            Stage::Repositories => host.repositories(),
            Stage::Accounts => host.accounts(root, accounts(plan.selection)),
        };
        match result {
            Ok(rows) if !rows.is_empty() => {
                if stage == Stage::Secrets {
                    vault_ready = rows_ready(&rows);
                }
                report.rows.extend(rows);
            }
            Ok(_) => report.rows.push(Row {
                name: stage.name().into(),
                status: "unverified".into(),
                detail: "the selected check returned no observations".into(),
            }),
            Err(detail) => report.rows.push(Row {
                name: stage.name().into(),
                status: "unverified".into(),
                detail,
            }),
        }
    }
    if plan.selection.is_empty() {
        report.rows.push(Row {
            name: "System State".into(),
            status: "ok".into(),
            detail: "no settings selected; no checks ran".into(),
        });
    }
    report
}

/// Observes the displayed checklist without mutating vault, repository, or setup state.
pub fn check_initial_setup(plan: &Plan) -> Result<Report, String> {
    check_initial_setup_observed(plan).result
}

/// Return Check rows only with the observation spanning all of their probes.
pub fn check_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<Report> {
    observe_read_only(state_root(), |root| {
        ensure_person_scope()?;
        let mut live = LiveCheck::prefetch(root, plan);
        Ok(check_with(&mut live, root, plan))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::macos_setup::Setting;

    #[derive(Default)]
    struct Fake {
        calls: Vec<Stage>,
        workstation: Option<Selection>,
        pending: Option<Stage>,
    }

    impl Fake {
        fn observe(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
            self.calls.push(stage);
            Ok(vec![readiness_row(
                stage.name(),
                self.pending != Some(stage),
                "injected observation".into(),
            )])
        }
    }

    impl InitialCheckHost for Fake {
        fn workstation(&mut self, _root: &Path, selected: Selection) -> Result<Vec<Row>, String> {
            self.workstation = Some(selected);
            self.observe(Stage::Workstation)
        }
        fn accounts(&mut self, _root: &Path, selected: Selection) -> Result<Vec<Row>, String> {
            self.workstation = Some(selected);
            self.observe(Stage::Accounts)
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.observe(Stage::Spotlight)
        }
        fn permissions(&mut self, _: &Plan, _: u64) -> Result<Vec<Row>, String> {
            self.observe(Stage::Permissions)
        }
        fn vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
            self.observe(Stage::Secrets)
        }
        fn repositories(&mut self) -> Result<Vec<Row>, String> {
            self.observe(Stage::Repositories)
        }
    }

    #[test]
    fn selected_checks_never_visit_unchecked_groups_or_change_progress() {
        let root = aterm_tempfile::tempdir().unwrap();
        let records = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
            "original-preferences.json",
        ];
        for record in records {
            std::fs::write(root.path().join(record), b"original durable record").unwrap();
        }
        for setting in Setting::ALL {
            let mut plan = Plan::from(Selection::none());
            plan.selection.set(setting, true);
            let mut host = Fake::default();
            let report = check_with(&mut host, root.path(), &plan);
            let expected = Stage::ALL
                .into_iter()
                .find(|stage| stage.selected(&plan))
                .unwrap();
            assert_eq!(host.calls, [expected]);
            assert_eq!(
                host.workstation,
                setting.is_workstation().then_some(plan.selection)
            );
            assert_eq!(report.action, Action::Check);
            assert!(!report.needs_attention());
            for record in records {
                assert_eq!(
                    std::fs::read(root.path().join(record)).unwrap(),
                    b"original durable record"
                );
            }
        }
        let mut host = Fake::default();
        let report = check_with(&mut host, root.path(), &Plan::from(Selection::none()));
        assert!(host.calls.is_empty());
        assert!(!report.needs_attention());
    }

    #[test]
    fn a_pending_selected_vault_holds_repository_credential_reads_back() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = check_with(&mut host, root.path(), &Plan::default());
        // The sign-outs need no vault: they run last either way.
        assert_eq!(host.calls, [&Stage::ALL[..4], &[Stage::Accounts]].concat());
        assert!(report.needs_attention());
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.name == Stage::Repositories.name() && row.status == "blocked")
        );
        host.calls.clear();
        host.pending = None;
        assert!(!check_with(&mut host, root.path(), &Plan::default()).needs_attention());
        assert_eq!(host.calls, Stage::ALL);
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    fn vault_status(membership: Membership) -> Status {
        Status {
            config: Some(client::Config {
                source: aterm_secrets::transport::Source::Url(
                    aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
                ),
                name: "alab".into(),
                signer: Some([7; 32]),
                followed: Vec::new(),
                host: "fixture".into(),
            }),
            machine: None,
            membership,
            names: Vec::new(),
        }
    }

    #[test]
    fn vault_check_refuses_other_sources_and_pending_membership_before_secret_file_reads() {
        let home = Path::new("/fixture");
        let plan = Plan {
            vault_url: "http://different.invalid:7484/v1/other".into(),
            ..Plan::default()
        };
        let status = vault_status(Membership::In { seq: 3, heard: 1 });
        let rows = vault_rows(
            &plan,
            home,
            &status,
            |_| panic!("unchecked files"),
            || panic!("unadmitted subscriber"),
        )
        .unwrap();
        assert_eq!(rows[0].status, "drift");
        let pending = vault_status(Membership::Pending);
        let rows = vault_rows(
            &Plan::default(),
            home,
            &pending,
            |_| panic!("pending membership cannot verify files"),
            || Ok(Subscribed::Running),
        )
        .unwrap();
        assert_eq!(rows[0].status, "blocked");
        assert!(!rows_ready(&rows));
    }

    #[test]
    fn vault_check_recovers_saved_pending_approval_code() {
        let code = "AB12-CD34";
        let mut status = vault_status(Membership::Pending);
        status.machine = Some((aterm_secrets::age::Recipient([7; 32]), code.into()));
        let rows = vault_rows(
            &Plan::default(),
            Path::new("/fixture"),
            &status,
            |_| panic!("pending membership cannot verify files"),
            || Ok(Subscribed::Running),
        )
        .unwrap();
        assert_eq!(rows[0].status, "blocked");
        assert!(
            rows[0].detail.contains(code),
            "Check must recover the durable machine approval code"
        );
        assert!(!rows_ready(&rows));
    }

    #[test]
    fn vault_check_does_not_show_another_vaults_approval_code() {
        let code = "AB12-CD34";
        let mut status = vault_status(Membership::Pending);
        status.machine = Some((aterm_secrets::age::Recipient([7; 32]), code.into()));
        let plan = Plan {
            vault_url: "http://different.invalid:7484/v1/other".into(),
            ..Plan::default()
        };
        let rows = vault_rows(
            &plan,
            Path::new("/fixture"),
            &status,
            |_| panic!("unadmitted files"),
            || panic!("unadmitted subscriber"),
        )
        .unwrap();
        assert_eq!(rows[0].status, "drift");
        assert!(rows.iter().all(|row| !row.detail.contains(code)));
    }

    #[test]
    fn vault_check_does_not_invent_approval_code_without_machine_identity() {
        let status = vault_status(Membership::Pending);
        assert!(status.machine.is_none());
        let rows = vault_rows(
            &Plan::default(),
            Path::new("/fixture"),
            &status,
            |_| panic!("pending membership cannot verify files"),
            || Ok(Subscribed::Running),
        )
        .unwrap();
        assert_eq!(rows[0].status, "blocked");
        assert!(!rows[0].detail.contains("code"));
        assert!(!rows_ready(&rows));
    }

    #[test]
    fn vault_check_admits_verified_rotation_and_rejects_unrelated_pins_before_file_reads() {
        let original = [7; 32];
        let mut status = vault_status(Membership::In { seq: 3, heard: 1 });
        let config = status.config.as_mut().unwrap();
        config.signer = Some([8; 32]);
        config.followed = vec![original];
        let pinned = |pin: [u8; 32]| Plan {
            vault_url: format!(
                "{}#{}",
                aterm_types::macos_setup::DEFAULT_VAULT_URL,
                aterm_secrets::b64::encode_padded(&pin)
            ),
            ..Plan::default()
        };
        let rows = vault_rows(
            &pinned(original),
            Path::new("/fixture"),
            &status,
            |_| Ok(true),
            || Ok(Subscribed::Running),
        )
        .unwrap();
        assert!(rows_ready(&rows));
        let rows = vault_rows(
            &pinned([9; 32]),
            Path::new("/fixture"),
            &status,
            |_| panic!("unrelated signer cannot admit secret file reads"),
            || panic!("unrelated signer cannot admit subscriber checks"),
        )
        .unwrap();
        assert_eq!(rows[0].status, "drift");
    }

    /// The vault is ready with its files written and a subscriber that runs, or
    /// one the person turned off: their choice, with the files as last synced,
    /// which the check names. NEGATIVE CONTROL: a subscriber that is down for no
    /// choice of theirs is not ready.
    #[test]
    fn vault_check_requires_materialized_files_and_a_running_subscriber() {
        let status = vault_status(Membership::In { seq: 3, heard: 1 });
        for (materialized, subscriber, ready) in [
            (false, Subscribed::Running, false),
            (true, Subscribed::Down, false),
            (true, Subscribed::Running, true),
            (true, Subscribed::TurnedOff, true),
            (false, Subscribed::TurnedOff, false),
        ] {
            let rows = vault_rows(
                &Plan::default(),
                Path::new("/fixture"),
                &status,
                |expected| {
                    assert_eq!(Some(expected), status.config.as_ref());
                    Ok(materialized)
                },
                || Ok(subscriber),
            )
            .unwrap();
            assert_eq!(rows_ready(&rows), ready, "{materialized} {subscriber:?}");
            assert_eq!(
                rows[1].detail.contains("by choice"),
                subscriber == Subscribed::TurnedOff
            );
        }
    }
}
