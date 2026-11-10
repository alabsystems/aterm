// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete an existing local default row from the native login, without enrolling a
//! new fleet account, changing credentials, or replacing authored account settings.

use super::*;
use aterm_toml::edit::{ArrayOfTables, DocumentMut, Item, TableLike};

fn identity(read: &mut impl FnMut(&Path) -> Option<String>, home: &Path) -> Option<String> {
    read(home)
        .map(|email| email.trim().to_owned())
        .filter(|email| accounts::email_refusal(email).is_none())
}

/// Used by explicit adoption and mutating startup/upkeep, never by read-only views.
/// `None` means no eligible existing row; the caller may continue normal adoption.
pub(in crate::harness) fn complete(
    env: &Env,
    asked: Option<&accounts::Row>,
    read: impl FnMut(&Path) -> Option<String>,
) -> Result<Option<String>, String> {
    complete_with(env, asked, read, &mut |store| {
        store
            .signin_within()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| accounts::SIGNIN_HELD.to_owned())
    })
}

pub(super) fn complete_manual(
    env: &Env,
    asked: &accounts::Row,
    read: impl FnMut(&Path) -> Option<String>,
    current: &dyn Fn() -> bool,
    queued: &mut dyn FnMut(accounts::SigninPosition),
) -> Result<Option<String>, String> {
    complete_with(env, Some(asked), read, &mut |store| {
        creation_signin_guard(store, current, queued)
    })
}

fn complete_with(
    env: &Env,
    asked: Option<&accounts::Row>,
    mut read: impl FnMut(&Path) -> Option<String>,
    take: &mut dyn FnMut(&accounts::Store) -> Result<accounts::SigninTurn, String>,
) -> Result<Option<String>, String> {
    let Some(path) = env.accounts_roster_for(Cli::Codex) else {
        return Ok(None);
    };
    let store = env
        .accounts_store_for(Cli::Codex)
        .ok_or_else(|| no_home_for(Cli::Codex))?;
    if store.sync_needs_recovery() {
        store
            .recover_sync_within(&accounts::LOCK_LADDER, &path)
            .map_err(SyncError::into_message)?;
    }
    if asked.is_none()
        && env
            .config
            .as_deref()
            .is_some_and(|config| !super::super::accounts_sync::sign_ins_on(config, Cli::Codex))
    {
        return Ok(None);
    }
    let Some(text) = accounts::roster_text(&path)? else {
        return Ok(None);
    };
    let roster = Roster::parse_for(Cli::Codex, &text)?;
    let Some(row) = asked.map_or_else(|| roster.row_for_login(None), |a| roster.row(&a.label))
    else {
        return Ok(None);
    };
    if row.dir.is_some() {
        return Err(format!(
            "`{}` is not this machine's default login",
            row.label
        ));
    }
    if row.held_by.is_some() {
        return if asked.is_some() {
            Err("the existing account is held; metadata is unchanged".into())
        } else {
            Ok(None)
        };
    }
    if asked.is_none() && row.email.is_some() {
        return Ok(None);
    }
    local_roster(&path)?;
    let Some(home) = env
        .home
        .as_ref()
        .map(|h| h.join(Cli::Codex.accounts().default_home))
    else {
        return Ok(None);
    };
    if !crate::ai::facts::codex_file_store(&home) {
        return Ok(None);
    }
    let Some(seen) = identity(&mut read, &home) else {
        return if asked.is_some() {
            Err("the default login named no ChatGPT identity; metadata is unchanged".into())
        } else {
            Ok(None)
        };
    };
    let mut completed = row.clone();
    if completed.email.is_none() {
        completed.email = Some(seen.clone());
    }
    // Capture source policy before taking the login locks. The final receipt
    // also checks the retained source under its native admission locks.
    let source = super::super::accounts_sync::codex_completion_source(env, &completed)?;
    let _signing = take(&store)?;
    let _held = store
        .acquire_bounded()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
    store.recover_sync_held(&_held, &path)?;
    let _auth = super::super::codex_app_login::AuthLock::try_take(&home.join("auth.json"))
        .map_err(|e| format!("the default login lock: {e}"))?
        .ok_or("the default login is being changed; metadata is unchanged")?;
    if accounts::roster_text(&path)?.as_deref() != Some(&text) {
        return Err("the roster changed during identity discovery; metadata is unchanged".into());
    }
    let mut state = store.load(env.now)?.0;
    store.hydrate_auth_contexts(&mut state, &roster);
    if accounts::condition(row, &state, true, env.now) == accounts::Condition::SignedOut {
        return if asked.is_some() {
            Err("the account is signed out; metadata is unchanged".into())
        } else {
            Ok(None)
        };
    }
    if let Some(asked) = asked {
        for (name, supplied, existing) in [
            ("email", asked.email.as_deref(), row.email.as_deref()),
            (
                "identity_email",
                asked.identity_email.as_deref(),
                row.identity_email.as_deref(),
            ),
            ("browser", asked.browser.as_deref(), row.browser.as_deref()),
        ] {
            if let Some(supplied) = supplied {
                let agrees = if name == "email" && existing.is_none() {
                    accounts::email_matches(supplied, &seen)
                } else {
                    existing == Some(supplied)
                };
                if !agrees {
                    return Err(format!("existing `{name}` differs; metadata is unchanged"));
                }
            }
        }
        if asked.vendor != accounts::Vendor::default() && asked.vendor != row.vendor {
            return Err("existing vendor settings differ; metadata is unchanged".into());
        }
    }
    if row
        .expected_identity_email()
        .is_some_and(|expected| !accounts::email_matches(expected, &seen))
    {
        return Err(
            "the default login differs from the existing account identity; metadata is unchanged"
                .into(),
        );
    }
    if source.held_by.is_some() {
        return if asked.is_some() {
            Err("the source holds this account; metadata is unchanged".into())
        } else {
            Ok(None)
        };
    }
    if let Some(fleet) = &source.fleet {
        for account in &fleet.roster.accounts {
            if account.label == row.label
                && (!accounts::email_matches(&account.email, &seen) || account.vendor != row.vendor)
            {
                return Err("the source assigns this label a different identity or realm; metadata is unchanged".into());
            }
        }
    }
    if !identity(&mut read, &home).is_some_and(|now| accounts::email_matches(&now, &seen)) {
        return Err(
            "the default login changed during identity discovery; metadata is unchanged".into(),
        );
    }
    if source != super::super::accounts_sync::codex_completion_source(env, &completed)?
        || accounts::roster_text(&path)?.as_deref() != Some(&text)
        || !crate::ai::facts::codex_file_store(&home)
        || (asked.is_none()
            && env.config.as_deref().is_some_and(|config| {
                !super::super::accounts_sync::sign_ins_on(config, Cli::Codex)
            }))
    {
        return Err(
            "account policy changed during identity discovery; metadata is unchanged".into(),
        );
    }
    if row.email.is_none() {
        local_roster(&path)?;
        let edited = with_email(&text, &row.label, &seen)?;
        store
            .transact_roster_admitted_held(
                &_held,
                env.now,
                &mut None,
                accounts::SyncTarget {
                    path: &path,
                    before: Some(&text),
                    after: Some(&edited),
                    mode: None,
                    fleet: None,
                },
                false,
                source.guard.as_ref(),
            )
            .map_err(SyncError::into_message)?;
    }
    Ok(Some(row.label.clone()))
}

fn with_email(text: &str, label: &str, email: &str) -> Result<String, String> {
    let before = Roster::parse_for(Cli::Codex, text)?;
    let mut expected = before.rows.clone();
    expected
        .iter_mut()
        .find(|row| row.label == label)
        .ok_or("missing row")?
        .email = Some(email.to_owned());
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("invalid roster: {e}"))?;
    match &mut doc["account"] {
        Item::ArrayOfTables(rows) => {
            let mut changed = ArrayOfTables::new();
            for mut row in rows.iter().cloned() {
                if row.get("label").and_then(Item::as_str) == Some(label) {
                    row.insert("email", Item::Value(email.into()));
                }
                changed.push(row);
            }
            *rows = changed;
        }
        Item::Value(aterm_toml::edit::Value::Array(rows)) => {
            for index in 0..rows.len() {
                if let Some(aterm_toml::edit::Value::InlineTable(row)) = rows.get_mut(index)
                    && row.get("label").and_then(aterm_toml::edit::Value::as_str) == Some(label)
                {
                    row.insert("email", Item::Value(email.into()));
                }
            }
        }
        _ => return Err("the roster has no account array".into()),
    }
    let edited = doc.to_string();
    if Roster::parse_for(Cli::Codex, &edited)?.rows != expected {
        return Err("the metadata edit changed other account settings".into());
    }
    Ok(edited)
}

/// Metadata completion owns only a direct local roster, never a symlink into a
/// source or another consumer's roster. Atomic replacement leaves hard-link aliases intact.
fn local_roster(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("default metadata completion requires a direct local roster; linked roster is unchanged".into());
    }
    Ok(())
}
