// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only observations of the displayed setup checklist.

use super::{
    Action, InitialHost, InitialSetupObserved, LiveInitial, NativeHost, PassPage, Path, Plan,
    Report, Row, Selection, Stage, access_revision_at, accounts, ensure_person_scope, execute,
    item_rows, observe_read_only, preferences, readiness_row, rows_ready, state_root, step_page,
};
use aterm_secrets::client::{self, Membership, Paths, Status};
use aterm_secrets::service::{self, Job, State};

trait InitialCheckHost {
    /// The Mac settings' check: its rows, and its page, which says them by System State row.
    fn workstation(&mut self, root: &Path, selected: Selection) -> Result<Report, String>;
    /// The sign-outs' check, as [`Self::workstation`].
    fn accounts(&mut self, root: &Path, selected: Selection) -> Result<Report, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String>;
    fn vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    fn repositories(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
}

#[derive(Default)]
struct LiveCheck {
    workstation: Option<Result<Report, String>>,
    accounts: Option<Result<Report, String>>,
    spotlight: Option<Result<Vec<Row>, String>>,
    vault: Option<Result<Vec<Row>, String>>,
}

impl LiveCheck {
    /// Read the plan's slow stages at once, each on its own thread: the Mac settings (then the
    /// sign-ins, one pass after the other), Spotlight and the vault. Aterm's permissions are
    /// read from the access record when the check reaches them.
    fn prefetch(root: &Path, plan: &Plan) -> Self {
        let check =
            |selected: Selection| execute(&mut NativeHost::selected(selected), root, Action::Check);
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
    fn workstation(&mut self, root: &Path, selected: Selection) -> Result<Report, String> {
        self.workstation
            .take()
            .unwrap_or_else(|| execute(&mut NativeHost::selected(selected), root, Action::Check))
    }

    fn accounts(&mut self, root: &Path, selected: Selection) -> Result<Report, String> {
        self.accounts
            .take()
            .unwrap_or_else(|| execute(&mut NativeHost::selected(selected), root, Action::Check))
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

    fn repositories(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        super::repositories::run(
            &aterm_machines::port::Live::new(),
            &LiveInitial::live().context()?,
            Stage::Secrets.selected(plan).then_some(plan),
            aterm_machines::model::Mode::Plan,
        )
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
        restore: None,
        page: None,
    };
    let mut page = PassPage::default();
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
        // A pass's own page says the Mac settings and the sign-outs by row (`Report::page`).
        let paged = |checked: Result<Report, String>| {
            checked.map_or_else(
                |why| (Err(why), None),
                |checked| (Ok(checked.rows), checked.page),
            )
        };
        let (result, own) = match stage {
            Stage::Workstation => paged(host.workstation(root, preferences(plan.selection))),
            Stage::Spotlight => (host.spotlight(), None),
            Stage::Permissions => (
                host.permissions(plan, access_revision_at(root).unwrap_or(0)),
                None,
            ),
            Stage::Secrets => (host.vault(plan), None),
            Stage::Repositories => (host.repositories(plan), None),
            Stage::Accounts => paged(host.accounts(root, accounts(plan.selection))),
        };
        match result {
            Ok(rows) if !rows.is_empty() => {
                if stage == Stage::Secrets {
                    vault_ready = rows_ready(&rows);
                }
                page.merge(own.unwrap_or_else(|| named_rows(&rows)));
                report.rows.extend(rows);
            }
            result => {
                let row = Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail: result
                        .err()
                        .unwrap_or_else(|| "the selected check returned no observations".into()),
                };
                page.merge(step_page(&row, stage.page_rows(&stage.subjects_in(plan))));
                report.rows.push(row);
            }
        }
    }
    if plan.selection.is_empty() {
        report.rows.push(Row {
            name: "System State".into(),
            status: "ok".into(),
            detail: "no settings selected; no checks ran".into(),
        });
    }
    report.page = Some(page);
    report
}

/// The page's rows of a stage's check `rows` (design §3.6): those that already name a System
/// State row, with no detail, since the page says them in its own words. An item's own line
/// (a preference key, a repository) is left to its row's reading, which the same Check takes.
fn named_rows(rows: &[Row]) -> PassPage {
    PassPage {
        outcome: aterm_types::macos_setup::PassOutcome::default(),
        rows: rows
            .iter()
            .filter(|row| {
                aterm_types::macos_setup::Subject::all()
                    .iter()
                    .any(|subject| subject.name() == row.name)
            })
            .map(|row| Row {
                detail: String::new(),
                ..row.clone()
            })
            .collect(),
    }
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
        /// A stage whose check fails as a whole.
        failing: Option<Stage>,
    }

    impl Fake {
        fn observe(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
            self.calls.push(stage);
            if self.failing == Some(stage) {
                return Err("injected: /usr/bin/defaults exited 1".into());
            }
            Ok(vec![readiness_row(
                stage.name(),
                self.pending != Some(stage),
                "injected observation".into(),
            )])
        }

        fn report(&mut self, root: &Path, stage: Stage) -> Result<Report, String> {
            Ok(Report {
                action: Action::Check,
                rows: self.observe(stage)?,
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
                restore: None,
                page: None,
            })
        }
    }

    impl InitialCheckHost for Fake {
        fn workstation(&mut self, root: &Path, selected: Selection) -> Result<Report, String> {
            self.workstation = Some(selected);
            self.report(root, Stage::Workstation)
        }
        fn accounts(&mut self, root: &Path, selected: Selection) -> Result<Report, String> {
            self.workstation = Some(selected);
            self.report(root, Stage::Accounts)
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
        fn repositories(&mut self, _: &Plan) -> Result<Vec<Row>, String> {
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

    /// A stage whose check fails as a whole is said on the page under the rows it covers, with
    /// none of its own words (design §3.6): `aterm doctor` keeps the stage's name and why.
    #[test]
    fn a_check_that_fails_as_a_whole_is_paged_under_its_rows() {
        let root = aterm_tempfile::tempdir().unwrap();
        for (stage, settings, said) in [
            (
                Stage::Spotlight,
                vec![Setting::Spotlight],
                vec!["Rust in Spotlight"],
            ),
            (
                Stage::Workstation,
                vec![Setting::CapsLock, Setting::Dock],
                vec![Setting::CapsLock.name(), Setting::Dock.name()],
            ),
            (
                Stage::Accounts,
                vec![Setting::Messages],
                vec![Setting::Messages.name()],
            ),
        ] {
            let mut plan = Plan::from(Selection::none());
            for setting in settings {
                plan.selection.set(setting, true);
            }
            let mut host = Fake {
                failing: Some(stage),
                ..Fake::default()
            };
            let report = check_with(&mut host, root.path(), &plan);
            assert!(
                report.rows.iter().any(|row| row.name == stage.name()
                    && row.detail == "injected: /usr/bin/defaults exited 1"),
                "{stage:?}: `aterm doctor` keeps the stage's own words"
            );
            let page: Vec<(&str, &str, &str)> = report
                .page
                .as_ref()
                .expect("a Check has a page")
                .rows
                .iter()
                .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
                .collect();
            let want: Vec<(&str, &str, &str)> = said
                .into_iter()
                .map(|name| (name, "unverified", ""))
                .collect();
            assert_eq!(page, want, "{stage:?}");
        }
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
