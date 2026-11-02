// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recover an absent account by a retained provider key and its current profile.

use super::*;

pub(super) struct Recovery {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) dir: String,
    key: super::super::profile::AccountKey,
}

pub(super) fn find(
    store: &accounts::Store,
    existing: &Roster,
    next: &Roster,
    state: &accounts::State,
    logins: &accounts::Logins,
    moves: &[(String, String)],
) -> Result<Vec<Recovery>, String> {
    let bound = accounts::bound(next, logins);
    let eligible: Vec<Row> = next
        .rows
        .iter()
        .filter(|row| row.dir.is_some() && !moves.iter().any(|(_, to)| to == &row.label))
        .filter(|row| {
            !existing.row(&row.label).is_some_and(|old| {
                same_account(old, row)
                    && bound
                        .row(&row.label)
                        .and_then(|row| row.dir.as_deref())
                        .is_some_and(|dir| matches!(held_for(Path::new(dir), row), Ok(Held::Own)))
            })
        })
        .cloned()
        .collect();
    let mut candidates: BTreeMap<String, Vec<Recovery>> = BTreeMap::new();
    for (from, record) in &state.accounts {
        if existing.row(from).is_some() || moves.iter().any(|(source, _)| source == from) {
            continue;
        }
        let Some(key) = record.key else {
            continue;
        };
        let binding = logins.get(from);
        let dir = binding
            .and_then(|binding| {
                binding
                    .dir
                    .clone()
                    .or_else(|| binding.was.last().cloned().flatten())
            })
            .or_else(|| {
                store
                    .login_dir(from)
                    .map(|path| path.to_string_lossy().into_owned())
            });
        let Some(dir) = dir else { continue };
        let Some((row, observed)) =
            super::super::profile::matching_row_key_in_home(Path::new(&dir), &eligible)
        else {
            continue;
        };
        if observed != key {
            continue;
        }
        let mut others = logins.clone();
        others.labels.remove(from);
        if accounts::naming_row(
            existing.rows.iter().filter(|old| !same_account(old, row)),
            &dir,
        )?
        .is_some()
            || accounts::retained_directory_owner(&others, &existing.rows, &row.label, &dir)?
                .is_some()
            || accounts::shared_staging_retained(logins, &dir)?
        {
            continue;
        }
        candidates
            .entry(row.label.clone())
            .or_default()
            .push(Recovery {
                from: from.clone(),
                to: row.label.clone(),
                dir,
                key,
            });
    }
    let mut found: Vec<_> = candidates
        .into_values()
        .filter_map(|mut found| if found.len() == 1 { found.pop() } else { None })
        .collect();
    // A new label may already have observations or pending work of its own. Do not erase
    // them or make every later sync refuse. A simultaneous move may free a destination;
    // prune to a fixed point so a blocked move cannot leave another targeting its source.
    loop {
        let sources: BTreeSet<_> = moves
            .iter()
            .map(|(from, _)| from.clone())
            .chain(found.iter().map(|recovery| recovery.from.clone()))
            .collect();
        let before = found.len();
        found.retain(|recovery| {
            sources.contains(&recovery.to)
                || !accounts::label_is_retained(state, logins, &recovery.to)
        });
        if before == found.len() {
            return Ok(found);
        }
    }
}

impl Recovery {
    pub(super) fn restore(&self, logins: &mut accounts::Logins, label: &str) {
        if let Some(binding) = logins.labels.get_mut(label) {
            binding.dir = Some(self.dir.clone());
        }
    }

    pub(super) fn recheck(&self, state: &accounts::State, next: &Roster) -> Result<(), String> {
        let current = next.row(&self.to).and_then(|row| {
            super::super::profile::matching_row_key_in_home(
                Path::new(&self.dir),
                std::slice::from_ref(row),
            )
        });
        if state
            .accounts
            .get(&self.from)
            .and_then(|account| account.key)
            != Some(self.key)
            || current.map(|(_, key)| key) != Some(self.key)
        {
            return Err(format!(
                "retained account identity changed while restoring `{}`; no new sync was published",
                self.to
            ));
        }
        Ok(())
    }
}
