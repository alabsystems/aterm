// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Current subscriber custody for bounded local projection admission.

use super::*;

/// Authority reads are separate from the final guard-only check, which may
/// run inside an EntryReader callback without acquiring source observations.
pub(crate) struct ReadValidation<'a> {
    pub(crate) current: &'a dyn Fn() -> Result<()>,
    pub(crate) custody: &'a dyn Fn() -> Result<()>,
}

fn lineage(selected: &Paths) -> Result<Vec<Paths>> {
    let mut paths = vec![selected.clone()];
    while let Some(parent) = linked(paths.last().ok_or_else(fail)?)? {
        if paths.len() > MAX_GENERATIONS {
            return Err(fail());
        }
        paths.push(parent.original);
    }
    Ok(paths)
}

fn sources(paths: &[Paths], deadline: Instant) -> Result<Vec<client::CommonsSourceRead>> {
    let mut backoff = Duration::from_millis(1);
    loop {
        within(deadline)?;
        let mut reads = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(read) =
                client::try_commons_source_read(path).map_err(crate::table::failure)?
            else {
                break;
            };
            reads.push(read);
        }
        if reads.len() == paths.len() {
            within(deadline)?;
            return Ok(reads);
        }
        // Do not wait on a writer while retaining an earlier source read.
        drop(reads);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::busy().context("subscriber vault source"));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

/// Retain exact source and control custody without opening a mutable journal.
/// The requested entry vault must be this generation or a retained ancestor.
/// Data-journal guards belong inside `action`, after these control guards.
pub(crate) fn with_retained_session<T>(
    paths: &Paths,
    entry_vault: &Paths,
    deadline: Instant,
    action: impl FnOnce(&Session<'_>, &client::VaultLook<'_>, &ReadValidation<'_>) -> Result<T>,
) -> Result<T> {
    let selected = observed_generation_until(paths, deadline)?;
    let lineage = lineage(&selected)?;
    if !lineage.contains(entry_vault) {
        return Err(Error::new(
            "entry vault is outside the retained source lineage",
        ));
    }
    let mut reads = sources(&lineage, deadline)?.into_iter();
    // Established custody and ancestor observation precede every control lock.
    let custody = lineage
        .iter()
        .rev()
        .map(|path| {
            Bootstrap::established_with_journal_until(path, deadline)
                .map(|(bootstrap, _)| bootstrap)
                .map_err(crate::table::failure)
        })
        .collect::<Result<Vec<_>>>()?;
    let observation = observe_generation(&selected, deadline, &|| deadline)?;
    let mut controls = custody
        .iter()
        .map(|bootstrap| {
            Journal::read_retained_until(bootstrap.journal_dir(), bootstrap.trust(), deadline)
                .map_err(crate::table::failure)
        })
        .collect::<Result<Vec<_>>>()?;
    if observed_generation_until(paths, deadline)? != selected
        || self::lineage(&selected)? != lineage
    {
        return Err(Error::snapshot_changed());
    }
    let bootstrap = custody.last().ok_or_else(fail)?;
    let (guard, journal) = controls.pop().ok_or_else(fail)?;
    let look = reads.next().ok_or_else(fail)?.into_look(bootstrap.paths());
    let ancestors = reads
        .zip(lineage.iter().skip(1))
        .map(|(read, path)| read.into_look(path))
        .collect::<Vec<_>>();
    let session = Session::retain_selected(bootstrap, journal, observation, deadline, look)?;
    let look = if entry_vault == session.paths() {
        session.retained_look().ok_or_else(fail)?
    } else {
        ancestors
            .iter()
            .find(|look| look.paths() == entry_vault)
            .ok_or_else(fail)?
    };
    let check_custody = || {
        within(deadline)?;
        guard.check()?;
        for (guard, _) in &controls {
            guard.check()?;
        }
        if !session
            .retained_look()
            .is_some_and(client::VaultLook::guards_source)
            || ancestors.iter().any(|look| !look.guards_source())
        {
            return Err(Error::snapshot_changed());
        }
        Ok(())
    };
    let revalidate = || {
        check_custody()?;
        if observed_generation_until(paths, deadline)? != selected
            || self::lineage(&selected)? != lineage
        {
            return Err(Error::snapshot_changed());
        }
        bootstrap
            .revalidate_custody_in(session.retained_look().ok_or_else(fail)?)
            .map_err(crate::table::failure)?;
        session.subscriber(deadline)
    };
    revalidate()?;
    action(
        &session,
        look,
        &ReadValidation {
            current: &revalidate,
            custody: &check_custody,
        },
    )
}
