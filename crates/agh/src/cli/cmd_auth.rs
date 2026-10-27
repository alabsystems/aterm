// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity verbs: `auth status`, `auth add`, and `auth remove`.

use std::path::Path;

use aterm_json::{Value, json};

use super::args::Invocation;
use super::cmd_whoami::{
    FROM_CONFIG, FROM_JOINED, FROM_VAULT, ambient_rows, profile_row, vault_rows,
};
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::config::ConfigFault;
use crate::error::{Error, Kind, Result};
use crate::identity::provision::{self, AddFrom, AddRequest};
use crate::identity::vault::Found;
use crate::identity::{self, tilde};
use crate::policy::{ChainStep, Presence, Router, Tier};
use crate::slug::Slug;

/// `platform` in `auth status`'s report.
#[cfg(unix)]
const PLATFORM: &str = "unix";
/// `platform` in `auth status`'s report.
#[cfg(not(unix))]
const PLATFORM: &str = "windows: anonymous only (file credentials need unix permission checks)";

fn check_name(name: &str) -> Result<()> {
    match crate::config::profile_name_fault(name) {
        Some(fault) => Err(Error::usage(fault)),
        None => Ok(()),
    }
}

fn add_request(inv: &Invocation) -> Result<AddRequest> {
    let usage = |why: &str| inv.usage(why);
    let name = inv.require(0, "NAME")?;
    check_name(name)?;
    if inv.global.anonymous {
        return Err(usage(
            "--anonymous sends no credential, and agh auth add binds the login with the new token: drop --anonymous",
        ));
    }
    let tier = inv
        .flag("--tier")
        .and_then(Tier::parse)
        .ok_or_else(|| usage("--tier wants dev, release or external"))?;
    let from = match (inv.flag("--file"), inv.has("--stdin")) {
        (Some(_), true) => return Err(usage("--file and --stdin contradict each other")),
        (None, false) => return Err(usage("name the token: --file PATH or --stdin")),
        (None, true) => AddFrom::Stdin,
        (Some(path), false) => {
            let path = Path::new(path);
            if path.is_absolute() {
                AddFrom::File(path.to_path_buf())
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|e| Error::could_not_run(format!("the current directory: {e}")))?;
                AddFrom::File(cwd.join(path))
            }
        }
    };
    let owners: Vec<String> = inv
        .flag_all("--owner")
        .into_iter()
        .map(str::to_string)
        .collect();
    let write = inv.has("--write");
    let drafts = inv.has("--drafts");
    if write && owners.is_empty() {
        return Err(usage("--write needs --owner OWNER"));
    }
    if drafts && !write {
        return Err(usage("--drafts vouches for a writer's token: add --write"));
    }
    for owner in &owners {
        Slug::new(owner, "repo")?;
    }
    Ok(AddRequest {
        name: name.to_string(),
        from,
        tier,
        owners,
        write,
        drafts,
    })
}

fn stdin_next(req: &AddRequest) -> String {
    let mut next = format!(
        "agh auth add {} --stdin --tier {}",
        req.name,
        req.tier.slug()
    );
    for owner in &req.owners {
        next.push_str(" --owner ");
        next.push_str(owner);
    }
    if req.write {
        next.push_str(" --write");
    }
    if req.drafts {
        next.push_str(" --drafts");
    }
    next.push_str(" < FILE");
    next
}

/// `agh auth add NAME (--file PATH | --stdin) --tier T [--owner O]… [--write [--drafts]]`: an
/// agent provisions as a person does, with or without a terminal (owner ruling 2026-10-26).
///
/// # Errors
///
/// Usage for its arguments, and for `--stdin` from a terminal, which would show the token as it
/// is typed; `provision::add`'s errors.
pub(crate) fn auth_add(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let req = add_request(inv)?;
    if cfg!(unix) && req.from == AddFrom::Stdin && ctx.stdin_is_terminal {
        return Err(Error::usage(
            "--stdin reads the token from a pipe or a file, never from a terminal, which would show it as it is typed",
        )
        .with_next(stdin_next(&req)));
    }
    let Some(dirs) = ctx.dirs.clone() else {
        return Err(Error::could_not_run(
            "no config directory: agh cannot provision here",
        ));
    };
    // Provisioning never adopts: a vault token adoption refuses or leaves out never stops an
    // agent from writing the agh.toml that routes past it.
    let gh = ctx.provisioning_client();
    let (added, meter) = provision::add_metered(&gh, &dirs, &req, &mut *ctx.stdin)?;
    let text = |s: &str| Value::from(ctx.redactor.apply(s));
    let value = json!({
        "fingerprint": added.fingerprint.as_str(),
        "from": FROM_CONFIG,
        "kind": added.kind.slug(),
        "login": text(&added.login),
        "owners": added.owners,
        "presence": "present",
        "profile": added.profile.as_str(),
        "remedy": null,
        "source": text(&format!("file:{}", tilde(&added.token_file, &dirs.home))),
        "tier": req.tier.slug(),
        "why": null,
    });
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Identity,
            value,
        },
        receipt: Receipt::new(&inv.verb_name()).metered(&meter),
    })
}

/// `agh auth remove NAME`; with no agh.toml, `dev` or `release` forgets the login vault
/// adoption recorded for that token. `joins_vault` says whether the vault's tokens route after
/// it (removing the last vault profile of an agh.toml makes it say `vault = true`).
///
/// # Errors
///
/// `provision::remove`'s errors.
pub(crate) fn auth_remove(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let name = inv.require(0, "NAME")?;
    let Some(dirs) = ctx.dirs.clone() else {
        return Err(Error::could_not_run(
            "no config directory: agh cannot provision here",
        ));
    };
    let removed = provision::remove(&dirs, name)?;
    let value = json!({
        "joins_vault": removed.joins_vault,
        "owners_edited": removed.owners_edited,
        "profile": removed.profile.as_str(),
        "token_file_deleted": removed.token_file_deleted,
    });
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Policy,
            value,
        },
        receipt: Receipt::new(&inv.verb_name()),
    })
}

type Read = Option<std::result::Result<crate::config::Validated, Vec<ConfigFault>>>;

/// agh.toml read and judged by `policy check`'s rules.
#[cfg(unix)]
fn read_config(dirs: &crate::config::Dirs) -> Read {
    use crate::config::{self, CONFIG_FILE};
    let read = config::read_config(&dirs.config, CONFIG_FILE, &dirs.home);
    config::faults_of(read, &dirs.home).map(|judged| judged.verdict)
}

/// Off unix agh never reads agh.toml (design §6.6): every read is anonymous.
#[cfg(not(unix))]
fn read_config(dirs: &crate::config::Dirs) -> Read {
    let _ = dirs;
    None
}

/// `agh auth status`: profile status, ambient tokens, and `agh.toml` faults. With no agh.toml,
/// or one that says `vault = true`, the vault's tokens as adoption finds them (design §6.10),
/// each row's `from` saying where it came from, and `routing` naming what routes: `agh.toml`,
/// `vault`, `agh.toml+vault` or `anonymous`.
///
/// # Errors
///
/// None of its own: an invalid agh.toml or adopted rows that break a rule (exit 1), an unusable
/// referenced profile or vault token (exit 4), and adoption's refusal are reports on stdout
/// with their error.
pub(crate) fn auth_status(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let (home, shown, read) = match &ctx.dirs {
        Some(dirs) => (
            dirs.home.clone(),
            Value::from(ctx.redactor.apply(&tilde(&dirs.config_file(), &dirs.home))),
            read_config(dirs),
        ),
        None => (std::path::PathBuf::new(), Value::Null, None),
    };
    let present = read.is_some();
    let mut profiles = Vec::new();
    let mut faults = Vec::new();
    let mut vault_faults = Vec::new();
    let mut vault_next: Option<String> = None;
    let mut unusable: Option<(String, &'static str, Option<String>)> = None;
    let mut refusal = None;
    let mut routing = if present { FROM_CONFIG } else { "anonymous" };
    let adoption = match &read {
        None => ctx.adoption().cloned(),
        Some(Err(found)) => {
            faults.clone_from(found);
            None
        }
        Some(Ok(validated)) => {
            let router = Router::from_validated(validated.clone());
            let mut named = router.profiles();
            named.sort_by(|a, b| a.name.cmp(&b.name));
            for profile in named {
                profiles.push(profile_row(
                    profile,
                    &*ctx.env,
                    &home,
                    &ctx.redactor,
                    FROM_CONFIG,
                ));
            }
            unusable = referenced_unusable(validated, &*ctx.env, &home);
            if validated.joins_vault() {
                ctx.adoption().cloned()
            } else {
                None
            }
        }
    };
    if let Some(adoption) = adoption {
        profiles.extend(vault_rows(&adoption, &*ctx.env, &home, &ctx.redactor));
        profiles.sort_by_key(|row| {
            row.get("profile")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        if adoption.adopted_any() {
            routing = if present { FROM_JOINED } else { FROM_VAULT };
        }
        if unusable.is_none() {
            unusable = adoption.vaulted().iter().find_map(|v| match &v.found {
                Found::Unusable { slug, remedy, .. } => {
                    Some((v.slot.profile().to_string(), *slug, remedy.clone()))
                }
                _ => None,
            });
        }
        vault_faults = adoption.faults().to_vec();
        vault_next = adoption.next_step();
        refusal = adoption.refusal().cloned();
    }
    let ambient = ambient_rows(&*ctx.env, &profiles);
    let listed: Vec<Value> = faults
        .iter()
        .chain(&vault_faults)
        .map(|fault| json!({"rule": fault.rule, "what": ctx.redactor.apply(&fault.what)}))
        .collect();
    let value = json!({
        "ambient": ambient,
        "config": shown,
        "faults": listed,
        "ok": faults.is_empty() && vault_faults.is_empty(),
        "platform": PLATFORM,
        "present": present,
        "profiles": profiles,
        "routing": routing,
    });
    let receipt = Receipt::new(&inv.verb_name());
    let answer = if let Some(error) = refusal {
        Answer::FailedWith { value, error }
    } else if !faults.is_empty() {
        let error = crate::config::invalid(
            format!("agh.toml breaks {} rule(s)", faults.len()),
            crate::config::POLICY_CHECK,
        );
        Answer::FailedWith { value, error }
    } else if !vault_faults.is_empty() {
        let error = crate::config::invalid(
            format!(
                "the vault's adopted rows break {} rule(s), so the tokens that bring them are \
                 not used",
                vault_faults.len()
            ),
            vault_next.as_deref().unwrap_or(crate::config::POLICY_CHECK),
        );
        Answer::FailedWith { value, error }
    } else if let Some((name, slug, remedy)) = unusable {
        let error = Error::new(
            Kind::Unauthenticated,
            format!("profile {name} is unusable: {slug}"),
        )
        .with_next(remedy.unwrap_or_else(|| "agh auth status".to_string()));
        Answer::FailedWith { value, error }
    } else {
        Answer::Object {
            noun: Noun::Policy,
            value,
        }
    };
    Ok(Outcome { answer, receipt })
}

fn referenced_unusable(
    validated: &crate::config::Validated,
    env: &dyn identity::EnvSource,
    home: &Path,
) -> Option<(String, &'static str, Option<String>)> {
    let mut referenced: Vec<&str> = Vec::new();
    let steps = validated
        .owners()
        .values()
        .flat_map(|owner| owner.read.iter())
        .chain(validated.default_read().iter());
    for step in steps {
        if let ChainStep::Profile(name) = step {
            referenced.push(name);
        }
    }
    referenced.extend(
        validated
            .owners()
            .values()
            .filter_map(|owner| owner.write.as_deref()),
    );
    referenced.sort_unstable();
    referenced.dedup();
    referenced.into_iter().find_map(|name| {
        let profile = validated.profiles().get(name)?;
        let error = identity::load(profile, env).err()?;
        match error.presence() {
            Presence::Unusable(slug) => Some((name.to_string(), slug, error.remedy(profile, home))),
            Presence::Present | Presence::Absent => None,
        }
    })
}
