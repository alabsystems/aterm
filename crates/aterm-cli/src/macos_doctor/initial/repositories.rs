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
            readback: None,
        });
    }
    result.map(reading)
}

fn reading(rows: Option<RepositoryRun>) -> Ran {
    match rows {
        Some(run) => Ran {
            failure: failure_note(&run.rows),
            token: want_token(&run.rows),
            rows: item_rows(run.rows),
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
            readback: None,
        },
    }
}

/// Why the repositories' item `rows` failed, where the step that stopped decided it: the
/// reading their item took, in System State's words (`GitHub can't be reached`), never the
/// engine's English, which `aterm doctor` and the log keep (System State design §3.6).
pub(super) fn failure_note(rows: &[aterm_machines::model::ItemReport]) -> Option<&'static str> {
    aterm_machines::init_observe::failure_note(&rows.iter().collect::<Vec<_>>())
}

/// Whether the repositories' item `rows` stopped because this Mac has no GitHub token to sign in
/// with: the file the vault's sync writes, or the one init.toml's `[github]` names, is not there
/// ([`aterm_machines::init_observe::Wait::Token`]).
pub(super) fn want_token(rows: &[aterm_machines::model::ItemReport]) -> bool {
    use aterm_machines::init_observe::{Reading, Wait};
    rows.iter()
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
