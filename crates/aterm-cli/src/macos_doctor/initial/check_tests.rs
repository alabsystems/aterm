// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// A check that panics reads as its stage's error, which says the panic, and every other
/// check still reads; refused a thread, the checks read the same in the caller. NEGATIVE
/// CONTROL: checks that answer read as they answered.
#[test]
fn a_check_that_panics_reads_as_its_stage_s_error_and_the_others_still_read() {
    fn read(stage: Stage) -> Vec<Row> {
        vec![readiness_row(stage.name(), true, "injected".into())]
    }
    let plan = Plan::from(Selection::all());
    let prefs = preferences(plan.selection);
    let outcome = |checks: &LiveCheck| {
        let said = |read: Option<Result<&[Row], &String>>| match read {
            Some(Ok(rows)) => rows[0].name.clone(),
            Some(Err(why)) => why.clone(),
            None => "unread".into(),
        };
        let rows = |read: &Option<Result<Vec<Row>, String>>| {
            said(read.as_ref().map(|read| read.as_deref()))
        };
        let report = |read: &Option<Result<Report, String>>| {
            said(
                read.as_ref()
                    .map(|read| read.as_ref().map(|report| report.rows.as_slice())),
            )
        };
        [
            report(&checks.workstation),
            report(&checks.accounts),
            rows(&checks.spotlight),
            rows(&checks.vault),
        ]
    };
    let fine = [
        Stage::Workstation.name(),
        Stage::Accounts.name(),
        Stage::Spotlight.name(),
        Stage::Secrets.name(),
    ];
    let lost = "the check panicked, so it read nothing: injected panic";
    for (ws_panics, spotlight_panics, read_as) in [
        (false, false, fine.map(str::to_owned)),
        (
            true,
            false,
            [lost, lost, fine[2], fine[3]].map(str::to_owned),
        ),
        (
            false,
            true,
            [fine[0], fine[1], lost, fine[3]].map(str::to_owned),
        ),
    ] {
        let check = move |selected: Selection| {
            assert!(!(ws_panics && selected == prefs), "injected panic");
            Ok(Report {
                action: Action::Check,
                rows: read(if selected == prefs {
                    Stage::Workstation
                } else {
                    Stage::Accounts
                }),
                manual_steps: Vec::new(),
                backup: std::path::PathBuf::new(),
                restore: None,
                page: None,
            })
        };
        let spotlight = move || {
            assert!(!spotlight_panics, "injected panic");
            Ok(read(Stage::Spotlight))
        };
        let vault = || Ok(read(Stage::Secrets));
        let threaded = LiveCheck::prefetch_with(&plan, check, spotlight, vault);
        assert_eq!(
            outcome(&threaded),
            read_as,
            "{ws_panics} {spotlight_panics}"
        );
        let here = crate::beside::tests::refusing(|| {
            LiveCheck::prefetch_with(&plan, check, spotlight, vault)
        });
        assert_eq!(outcome(&here), read_as, "in the caller");
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

/// EVERY ROW OF A CHECK CARRIES ITS TIME: each stage's readings, and the note when nothing
/// is selected (presence only, never a budget).
#[test]
fn every_check_row_carries_its_time() {
    let root = aterm_tempfile::tempdir().unwrap();
    for plan in [Plan::default(), Plan::from(Selection::none())] {
        let report = check_with(&mut Fake::default(), root.path(), &plan);
        assert!(!report.rows.is_empty());
        assert!(
            report.rows.iter().all(|row| row.elapsed_ms.is_some()),
            "{}",
            report.to_text()
        );
    }
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
            mirrors: Vec::new(),
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
