// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only observations of the displayed setup checklist.

use super::{
    Action, InitialHost, InitialSetupObserved, LiveInitial, NativeHost, PassPage, Path, Plan,
    Report, Row, RowClock, Selection, Stage, access_revision_at, accounts, ensure_person_scope,
    execute, item_rows, observe_read_only, preferences, readiness_row, rows_ready, state_root,
    step_page,
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
        Self::prefetch_with(plan, check, live_spotlight, || current_vault_rows(plan))
    }

    /// [`Self::prefetch`] with its checks given: each on a thread of its own, or in the caller
    /// where no thread starts ([`crate::beside`]). A check that panics reads as its stage's
    /// error ([`panicked`]), and the others still read.
    fn prefetch_with(
        plan: &Plan,
        check: impl Fn(Selection) -> Result<Report, String> + Copy + Send,
        spotlight: impl FnOnce() -> Result<Vec<Row>, String> + Clone + Send,
        vault: impl FnOnce() -> Result<Vec<Row>, String> + Clone + Send,
    ) -> Self {
        let (prefs, signins) = (preferences(plan.selection), accounts(plan.selection));
        let ws_selected = Stage::Workstation.selected(plan);
        let accounts_selected = Stage::Accounts.selected(plan);
        std::thread::scope(|s| {
            let ws = (ws_selected || accounts_selected).then(|| {
                crate::beside::start(s, "aterm-check-workstation", move || {
                    (
                        ws_selected.then(|| check(prefs)),
                        accounts_selected.then(|| check(signins)),
                    )
                })
            });
            let sp = Stage::Spotlight
                .selected(plan)
                .then(|| crate::beside::start(s, "aterm-check-spotlight", spotlight));
            let vault = Stage::Secrets
                .selected(plan)
                .then(|| crate::beside::start(s, "aterm-check-vault", vault));
            let (workstation, accounts) = ws
                .map(|ws| {
                    ws.answer_or(|panic| {
                        let why = panicked(&*panic);
                        (
                            ws_selected.then(|| Err(why.clone())),
                            accounts_selected.then_some(Err(why)),
                        )
                    })
                })
                .unwrap_or_default();
            Self {
                workstation,
                accounts,
                spotlight: sp.map(|sp| sp.answer_or(|panic| Err(panicked(&*panic)))),
                vault: vault.map(|vault| vault.answer_or(|panic| Err(panicked(&*panic)))),
            }
        })
    }
}

/// The error a check that panicked reads as: it read nothing, and its panic's words say why.
fn panicked(panic: &(dyn std::any::Any + Send)) -> String {
    let said = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
    match said {
        Some(said) => format!("the check panicked, so it read nothing: {said}"),
        None => "the check panicked, so it read nothing".into(),
    }
}

/// The Spotlight stage's check.
fn live_spotlight() -> Result<Vec<Row>, String> {
    aterm_machines::init::run_initial_spotlight(
        &aterm_machines::port::Live::new(),
        &LiveInitial::live().context()?,
        aterm_machines::model::Mode::Plan,
    )
    .map(item_rows)
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
        self.spotlight.take().unwrap_or_else(live_spotlight)
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
            None,
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
            elapsed_ms: None,
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
    // Each stage's rows its producer did not time carry the stage's time.
    let mut clock = RowClock::start();
    for stage in Stage::ALL {
        clock.lap(&mut report.rows);
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
                    elapsed_ms: None,
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
            elapsed_ms: None,
        });
    }
    clock.lap(&mut report.rows);
    report.page = Some(page);
    report
}

/// The page's rows of a stage's check `rows` (design §3.6): those that already name a System
/// State row, with no detail, since the page says them in its own words. An item's own line
/// (a preference key, a repository) is left to its row's reading, which the same Check takes.
fn named_rows(rows: &[Row]) -> PassPage {
    PassPage {
        readback: super::WorkstationReadback::EMPTY,
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
#[path = "check_tests.rs"]
mod tests;
