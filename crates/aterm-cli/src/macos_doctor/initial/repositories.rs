// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The selected vault's repository inputs and their admission to GitHub work.

use super::{Plan, Row, Stage, item_rows, readiness_row};
use aterm_machines::init_github::{RepositoryReadback, RepositoryRun};
use aterm_machines::items::Ctx;
use aterm_machines::model::Mode;
use aterm_machines::port::System;
use std::sync::Arc;

type Current = Arc<dyn Fn() -> Result<bool, String> + Send + Sync>;

pub(super) fn run(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<Vec<Row>, String> {
    items_current(sys, ctx, vault, mode, false, None, readback).map(|rows| reading(rows).rows)
}

/// What a Repositories stage [`run_reading_guarded`] ran came to.
pub(super) struct Ran {
    pub rows: Vec<Row>,
    /// The reason a part that failed gives ([`failure_note`]).
    pub failure: Option<&'static str>,
    /// It stopped for want of a GitHub token to sign in with ([`want_token`]).
    pub token: bool,
    /// What its acquisition was left waiting on, where the step that stopped typed it
    /// ([`left_wait`]).
    pub wait: Option<aterm_machines::init_observe::Wait>,
    /// What the run read of the GitHub repositories row, dated from before it read anything
    /// ([`crate::macos_doctor::repositories_readback`]): none from a run that read none.
    pub observed: crate::macos_doctor::WorkstationReadback,
    /// Actual native empty-origin evidence owned only by this setup host/pass.
    pub readback: Option<Arc<RepositoryReadback>>,
}

/// Run with the current setup choice checked at each clone boundary, including when the
/// Secrets group is left as it is and the inputs are this account's local files.
pub(super) fn run_reading_guarded(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<Ran, String> {
    // Dated from before the run reads anything (design §3.5).
    let at = sys.now();
    let current: Current = Arc::new(current);
    let result = items_current(
        sys,
        ctx,
        vault,
        mode,
        false,
        Some(Arc::clone(&current)),
        None,
    );
    if let Err(why) = current().and_then(|ready| {
        ready
            .then_some(())
            .ok_or_else(|| "repository setup was held while cloning".to_owned())
    }) {
        return Ok(Ran {
            rows: vec![Row {
                name: Stage::Repositories.name().into(),
                status: super::WAITING.into(),
                detail: why,
                elapsed_ms: None,
            }],
            failure: None,
            token: false,
            wait: None,
            observed: crate::macos_doctor::WorkstationReadback::EMPTY,
            readback: None,
        });
    }
    result.map(|run| {
        let observed = run
            .as_ref()
            .map_or(crate::macos_doctor::WorkstationReadback::EMPTY, |run| {
                crate::macos_doctor::repositories_readback(&run.rows, at)
            });
        Ran {
            observed,
            ..reading(run)
        }
    })
}

fn reading(rows: Option<RepositoryRun>) -> Ran {
    match rows {
        Some(run) => Ran {
            failure: failure_note(&run.rows),
            token: want_token(&run.rows),
            wait: left_wait(&run.rows),
            observed: crate::macos_doctor::WorkstationReadback::EMPTY,
            rows: item_rows(run.rows.into_iter().map(refresh_left_for_later).collect()),
            readback: run.readback.map(Arc::new),
        },
        None => Ran {
            rows: vec![readiness_row(
                Stage::Repositories.name(),
                false,
                "the selected secrets vault changed or is still syncing; repository inputs wait for a complete version".into(),
            )],
            failure: None,
            token: false,
            wait: None,
            observed: crate::macos_doctor::WorkstationReadback::EMPTY,
            readback: None,
        },
    }
}

/// The refresh of the checkouts already here (`init.github.refresh@<user>`) as System State's
/// stage counts it: a fault of it, or a read-back that could not confirm it, is work left for a
/// later pass (the subscriber's `sync.refresh` runs it again), so the stage parks on it and
/// never fails or retries the GitHub repositories setting for it; that setting stands as their
/// acquisition reads ([`aterm_machines::init_observe::observation`]). The refresh's reason stays
/// its row's words, which `aterm doctor` and the log keep. Every other row is as its item
/// reported it.
fn refresh_left_for_later(
    mut row: aterm_machines::model::ItemReport,
) -> aterm_machines::model::ItemReport {
    if aterm_machines::init_github::refresh_row(&row)
        && matches!(super::row_status(&row), "error" | "blocked" | "unverified")
    {
        row.outcome = aterm_machines::model::Outcome::Deferred;
    }
    row
}

/// The rows of `rows` the GitHub repositories setting stands on: all but the refresh of the
/// checkouts already here ([`aterm_machines::init_github::refresh_row`]), whose fault or wait
/// is its own row's alone.
fn acquisition(
    rows: &[aterm_machines::model::ItemReport],
) -> Vec<&aterm_machines::model::ItemReport> {
    rows.iter()
        .filter(|row| !aterm_machines::init_github::refresh_row(row))
        .collect()
}

/// Why the repositories' acquisition `rows` failed, where the step that stopped decided it: the
/// reading their item took, in System State's words (`GitHub can't be reached`), never the
/// engine's English, which `aterm doctor` and the log keep (System State design §3.6). A
/// refresh of the checkouts already here gives none ([`acquisition`]).
pub(super) fn failure_note(rows: &[aterm_machines::model::ItemReport]) -> Option<&'static str> {
    aterm_machines::init_observe::failure_note(&acquisition(rows))
}

/// What the repositories' acquisition `rows` were left waiting on: the first part's wait, whose
/// fact ([`aterm_machines::init_observe::Wait::fact`]) is what the park after the stage watches.
/// It is the wait the step that stopped typed ([`aterm_machines::init_observe::Reading::Waits`]),
/// or [`Wait::InTheWay`](aterm_machines::init_observe::Wait::InTheWay) where the part needs a
/// person because work setup preserves, another origin or something else sits at a
/// repository's destination (its counts' `preserved`): only a change there moves it. A refresh
/// of the checkouts already here gives none ([`acquisition`]), nor does a part that failed.
pub(super) fn left_wait(
    rows: &[aterm_machines::model::ItemReport],
) -> Option<aterm_machines::init_observe::Wait> {
    use aterm_machines::init_observe::{Reading, Wait};
    use aterm_machines::model::Outcome;
    acquisition(rows)
        .into_iter()
        .find_map(|row| match Reading::of(row)? {
            Reading::Waits(wait) => Some(wait),
            Reading::Repositories { counts, .. }
                if counts.preserved > 0 && row.outcome == Outcome::Person =>
            {
                Some(Wait::InTheWay)
            }
            _ => None,
        })
}

/// Whether the repositories' acquisition `rows` stopped because this Mac has no GitHub token to
/// sign in with: the file the vault's sync writes, or the one init.toml's `[github]` names, is
/// not there ([`aterm_machines::init_observe::Wait::Token`]).
pub(super) fn want_token(rows: &[aterm_machines::model::ItemReport]) -> bool {
    use aterm_machines::init_observe::{Reading, Wait};
    acquisition(rows)
        .into_iter()
        .any(|row| Reading::of(row) == Some(Reading::Waits(Wait::Token)))
}

/// The GitHub repositories row, read as [`run`] reads them for `vault`. Where the ALab vault is off
/// this network (`not_here`) and this Mac is not in `vault`, they are read on this Mac's own GitHub
/// credentials, as their stage then runs them, and wait for the vault's token, as the stage does,
/// only where this Mac has none to sign in with.
pub(super) fn observe(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    not_here: bool,
) -> Result<aterm_types::macos_setup::Observation, String> {
    use aterm_types::macos_setup::{Observation, Setting, Standing, Subject};
    Ok(match items(sys, ctx, vault, Mode::Plan, not_here)? {
        Some(rows) => aterm_machines::init_observe::observation(
            Setting::Repositories,
            &rows.iter().collect::<Vec<_>>(),
        ),
        None => Observation {
            standing: Standing::Waiting,
            note: Some("Waits for the vault to finish syncing this Mac".into()),
            ..Observation::unknown(Subject::Setting(Setting::Repositories))
        },
    })
}

/// The repositories' item rows, read through `vault` where one is given, else on this Mac's own
/// GitHub credentials: `None` while that vault is not this Mac's, changed, or is still syncing.
/// Where it is not this Mac's and the Mac signs in on its own credentials instead (`own`), they are
/// read with those, `None` where it has no token to sign in with ([`want_token`]).
fn items(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    own: bool,
) -> Result<Option<RepositoryRun>, String> {
    items_current(sys, ctx, vault, mode, own, None, None)
}

fn local_items(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    current: Option<Current>,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<RepositoryRun, String> {
    if current.is_some()
        && let Some(rows) = withdrawal_items(sys, ctx, mode, current.clone(), None)?
    {
        return Ok(rows);
    }
    if current.is_none() && readback.is_none() {
        return aterm_machines::init_github::run_initial_repositories(sys, ctx, mode);
    }
    let work = if readback.is_some() {
        aterm_secrets::converge::RepositoryWork::Repair
    } else {
        aterm_secrets::converge::RepositoryWork::Refresh
    };
    let inputs = aterm_machines::init_github::InitialRepositoryInputs::capture_ready(sys, ctx)?
        .with_repository_readback(readback);
    aterm_machines::init_github::run_initial_repositories_captured(
        sys,
        ctx,
        mode,
        work,
        aterm_machines::init_github::RepositorySteps::Both,
        inputs,
        move || current.as_ref().map_or(Ok(true), |current| current()),
    )
}

fn withdrawal_items(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    current: Option<Current>,
    selected: Option<(
        &aterm_secrets::client::Paths,
        &aterm_secrets::client::Config,
    )>,
) -> Result<Option<RepositoryRun>, String> {
    let Ok(Some(withdrawal)) =
        aterm_machines::init_github::RepositoryCredentialWithdrawal::capture(sys, ctx)
    else {
        return Ok(None);
    };
    let selected = selected.map(|(paths, config)| (paths.clone(), config.clone()));
    aterm_machines::init_github::run_repository_credential_withdrawal(
        sys,
        ctx,
        mode,
        withdrawal,
        move || {
            if let Some(current) = &current
                && !current()?
            {
                return Ok(false);
            }
            match &selected {
                Some((paths, expected)) => Ok(aterm_secrets::client::config(paths)
                    .map_err(|why| why.to_string())?
                    .as_ref()
                    == Some(expected)),
                None => Ok(true),
            }
        },
    )
    .map(|rows| {
        Some(RepositoryRun {
            rows,
            readback: None,
        })
    })
}

fn items_current(
    sys: &dyn System,
    ctx: &Ctx,
    vault: Option<&Plan>,
    mode: Mode,
    own: bool,
    current: Option<Current>,
    readback: Option<Arc<RepositoryReadback>>,
) -> Result<Option<RepositoryRun>, String> {
    let Some(vault) = vault else {
        return local_items(sys, ctx, mode, current, readback).map(Some);
    };
    let [user] = ctx.users.as_slice() else {
        return Err("repository setup needs exactly one console account".into());
    };
    let paths = aterm_secrets::client::Paths::for_home(std::path::Path::new(&user.home));
    let configured = aterm_secrets::client::config(&paths).map_err(|error| error.to_string())?;
    let selected = aterm_machines::init_secrets::selected_vault(
        &vault.vault_url,
        &paths.home,
        configured.as_ref(),
    )?;
    let Some(expected) = selected else {
        if !own {
            return Ok(None);
        }
        return local_items(sys, ctx, mode, current, readback)
            .map(|rows| (!want_token(&rows)).then_some(rows));
    };
    // This selected vault still admits its current local empty/disabled intent when the
    // paired credential has not arrived. This path grants no ordinary repository effects.
    if let Some(rows) = withdrawal_items(sys, ctx, mode, current.clone(), Some((&paths, expected)))?
    {
        return Ok(Some(rows));
    }
    let entries = aterm_machines::init_github::uses_vault_inventory(sys, ctx)?
        .then_some(aterm_machines::init_github::VAULT_INPUT_ENTRIES.as_slice());
    let read = || {
        aterm_machines::init_github::InitialRepositoryInputs::capture(sys, ctx)
            .map_err(aterm_secrets::Error::new)
    };
    let captured = match entries {
        Some(entries) => aterm_secrets::client::with_materialization_ready_for_entries(
            &paths, expected, entries, read,
        ),
        None => aterm_secrets::client::with_materialization_ready(&paths, expected, read),
    }
    .map_err(|error| error.to_string())?;
    let Some(captured) = captured else {
        return Ok(None);
    };
    let work = if readback.is_some() {
        aterm_secrets::converge::RepositoryWork::Repair
    } else {
        aterm_secrets::converge::RepositoryWork::Refresh
    };
    aterm_machines::init_github::run_initial_repositories_captured(
        sys,
        ctx,
        mode,
        work,
        aterm_machines::init_github::RepositorySteps::Both,
        captured.inputs.with_repository_readback(readback),
        move || {
            if let Some(current) = &current
                && !current()?
            {
                return Ok(false);
            }
            captured
                .snapshot
                .is_current(&paths)
                .map_err(|error| error.to_string())
        },
    )
    .map(Some)
}

#[cfg(test)]
#[path = "repositories_tests.rs"]
mod tests;
