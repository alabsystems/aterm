// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Composition of root-approved account data with the existing local login
//! workflow. Shared configuration never becomes an unchecked local roster row.
//!
//! Traced by the accounts keeper: what this file reads of the file system itself, it reads
//! through `aterm_uds::trace::fs` (census `traced-reads`).

use aterm_agent::harness::{
    accounts,
    cli::{
        self,
        shared_renewal::{Authority, Grant},
    },
};
use aterm_db::accounts::activations::Activations;
use aterm_secrets::client::{self, Config, Paths};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, process::ExitCode};

#[path = "shared_native.rs"]
mod native;
#[path = "shared_login_provider.rs"]
mod providers;
#[path = "shared_login_recovery.rs"]
mod recovery;
#[path = "repository_catalog.rs"]
mod repository_catalog;

const USAGE: &str = "aterm harness accounts shared-renew <account> --account <digest> --decision <digest> --request <nonce> [--config <aterm.toml>]\nRenews one existing local Claude OAuth row, named by its account (its slug, its key, or `claude <address>`), using an exact current root grant.\n";

pub(super) fn install_automatic() {
    aterm_agent::harness::accounts_sync::shared_renewal::install_authority(automatic_authority);
    aterm_agent::harness::accounts_sync::shared_renewal::install_checkpoint_authority(
        checkpoint_authority,
    );
    aterm_agent::harness::accounts_sync::shared_renewal::install_discovery(automatic_candidates);
    recovery::install();
    native::install();
}

fn automatic_candidates(
    home: &std::path::Path,
) -> Result<Vec<aterm_agent::harness::accounts_sync::shared_renewal::Candidate>, String> {
    let paths = Paths::for_home(home);
    let Some(expected) = client::config(&paths).map_err(|why| why.to_string())? else {
        return Ok(Vec::new());
    };
    let db = Activations::at(paths.clone());
    let mut candidates = Vec::new();
    for (account, decision) in db.local_preparations().map_err(|why| why.to_string())? {
        let approved = Approved {
            paths: paths.clone(),
            expected: expected.clone(),
            account,
            decision,
        };
        if let Ok(candidate) = providers::candidate(std::sync::Arc::new(approved)) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn current_authority_with_prepared(
    home: &std::path::Path,
    source: accounts::shared::Source,
    row: &accounts::Row,
) -> Result<
    (
        std::sync::Arc<Approved>,
        aterm_db::accounts::activations::Preparation,
    ),
    String,
> {
    let paths = Paths::for_home(home);
    let expected = client::config(&paths)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "shared renewal requires an enrolled vault".to_owned())?;
    let aterm_db::accounts::activations::State::Ready(decision) = Activations::at(paths.clone())
        .state(source.account)
        .map_err(|e| e.to_string())?
    else {
        return Err("shared renewal has no unique current root preparation".into());
    };
    let approved = Approved {
        paths,
        expected,
        account: source.account,
        decision,
    };
    let prepared = approved.prepared()?;
    if approved.read_from(&prepared, row)?.source != source {
        return Err("shared renewal belongs to a different retained network".into());
    }
    Ok((std::sync::Arc::new(approved), prepared))
}

fn automatic_authority(
    home: &std::path::Path,
    source: accounts::shared::Source,
    row: &accounts::Row,
) -> Result<aterm_agent::harness::accounts_sync::shared_renewal::CheckedAuthority, String> {
    let (approved, prepared) = current_authority_with_prepared(home, source, row)?;
    if !providers::ordinary_renewal(&prepared) {
        return Err("shared account requires its prepared-provider login path".into());
    }
    Ok(approved)
}

fn checkpoint_authority(
    home: &std::path::Path,
    source: accounts::shared::Source,
    row: &accounts::Row,
) -> Result<aterm_agent::harness::accounts_sync::shared_renewal::CheckedAuthority, String> {
    let (approved, prepared) = current_authority_with_prepared(home, source, row)?;
    if !providers::claude_checkpoint(&prepared) {
        return Err("shared checkpoint is not a supported Claude preparation".into());
    }
    Ok(approved)
}

struct Approved {
    paths: Paths,
    expected: Config,
    account: [u8; 32],
    decision: [u8; 32],
}

fn requirement<'a>(value: &'a aterm_json::Value, key: &str) -> Result<Option<&'a str>, String> {
    let Some(detail) = value.get("details").and_then(|details| details.get(key)) else {
        return Ok(None);
    };
    if detail["kind"].as_str() != Some("requirement") {
        return Err(format!("shared renewal detail {key} must be a requirement"));
    }
    detail["value"]
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("invalid shared renewal detail {key}"))
}

fn has_unsupported_requirements(value: &aterm_json::Value, supported: &[&str]) -> bool {
    value
        .get("details")
        .and_then(|details| details.as_object())
        .is_some_and(|details| {
            details.iter().any(|(key, detail)| {
                detail["kind"].as_str() == Some("requirement") && !supported.contains(&key.as_str())
            })
        })
}

impl Authority for Approved {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        self.read(row)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        if self.read(row)? != *grant {
            return Err("shared renewal approval changed".into());
        }
        Ok(())
    }
    fn request_recovery(
        &self,
        grant: &Grant,
        row: &accounts::Row,
        intent: &accounts::shared::Intent,
    ) -> Result<(), String> {
        let prepared = self.prepared()?;
        if self.read_from(&prepared, row)? != *grant {
            return Err("shared renewal approval changed".into());
        }
        if !matches!(
            &prepared.decision().request().action,
            aterm_secrets::commons::accounts::Action::Prepare(terms) if terms.intent.is_some()
        ) {
            return Ok(());
        }
        if intent.source != grant.source
            || intent.decision != grant.decision
            || intent.configuration != grant.configuration
        {
            return Err("shared recovery request differs from the retained preparation".into());
        }
        aterm_db::accounts::recovery::Recovery::at(self.paths.clone())
            .request(self.account, self.decision, intent.request)
            .map_err(|why| why.to_string())
    }
}

struct Command {
    /// The account word that names the row ([`aterm_agent::harness::account_names::row_named`]).
    account_word: String,
    account: [u8; 32],
    decision: [u8; 32],
    request: [u8; 16],
    config: Option<PathBuf>,
}

fn digest<const N: usize>(text: &str) -> Result<[u8; N], String> {
    let mut digest = [0; N];
    if !aterm_codec::hex::decode_into(text.as_bytes(), &mut digest) || digest == [0; N] {
        return Err("invalid shared renewal identifier".into());
    }
    Ok(digest)
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    // The operands before the first option name one account: its slug, its key, or its provider
    // word and address in either form (`claude ada@x.example`, `claude:ada@x.example`).
    let operands = args
        .iter()
        .take_while(|arg| !arg.to_str().is_some_and(|arg| arg.starts_with("--")))
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| "shared renewal's account must be UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [account_word] = aterm_agent::harness::account_names::words(&operands)
        .try_into()
        .map_err(|_| "shared renewal names one account".to_owned())?;
    let mut options = BTreeMap::new();
    let (pairs, remainder) = args[operands.len()..].as_chunks::<2>();
    for pair in pairs {
        let key = pair[0]
            .to_str()
            .filter(|s| matches!(*s, "--account" | "--decision" | "--request" | "--config"))
            .ok_or_else(|| "unknown shared renewal option".to_owned())?;
        let value = pair[1]
            .to_str()
            .ok_or_else(|| "shared renewal options must be UTF-8".to_owned())?;
        if options.insert(key, value).is_some() {
            return Err("duplicate shared renewal option".into());
        }
    }
    if !remainder.is_empty() {
        return Err("shared renewal option is missing its value".into());
    }
    let mut take = |key| {
        options
            .remove(key)
            .ok_or_else(|| "missing shared renewal identifier".to_owned())
    };
    let account = digest(take("--account")?)?;
    let decision = digest(take("--decision")?)?;
    let request = digest(take("--request")?)?;
    let config = options.remove("--config").map(PathBuf::from);
    Ok(Command {
        account_word,
        account,
        decision,
        request,
        config,
    })
}

pub(super) fn try_run(args: &[OsString]) -> Option<ExitCode> {
    if let Some(result) = native::try_run(args) {
        return Some(result);
    }
    if args.first().is_none_or(|a| a != "accounts")
        || args.get(1).is_none_or(|a| a != "shared-renew")
    {
        return None;
    }
    if args[2..].iter().any(|arg| arg == "--help" || arg == "-h") {
        return Some(aterm_uds::stdout::answer_exit("aterm", USAGE, 0));
    }
    let result: Result<ExitCode, String> = (|| {
        let command = parse(&args[2..])?;
        let mut env = cli::Env::from_process()?;
        if let Some(config) = command.config {
            env.config = Some(config);
        }
        let paths = Paths::live().map_err(|e| e.to_string())?;
        let expected = client::config(&paths)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "shared renewal requires an enrolled vault".to_owned())?;
        let authority = Approved {
            paths,
            expected,
            account: command.account,
            decision: command.decision,
        };
        if !providers::ordinary_renewal(&authority.prepared()?) {
            return Err("shared account requires its prepared-provider login path".into());
        }
        Ok(cli::shared_renewal::run(
            &env,
            &cli::LiveAccounts::default(),
            &command.account_word,
            command.request,
            &authority,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        ))
    })();
    Some(match result {
        Ok(code) => code,
        Err(why) => {
            aterm_uds::stdout::answer_exit("aterm shared-renew", &format!("{why}\n{USAGE}"), 2)
        }
    })
}

#[cfg(test)]
#[path = "shared_login_tests.rs"]
mod tests;
