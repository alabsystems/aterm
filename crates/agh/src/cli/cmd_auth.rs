// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity verbs: `auth status`, `auth add`, and `auth remove`.

use std::path::Path;

use aterm_json::{Value, json};

use super::args::Invocation;
use super::cmd_whoami::{ambient_rows, profile_row};
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::config::ConfigFault;
use crate::error::{Error, Kind, Result};
use crate::identity::provision::{self, AddFrom, AddRequest};
use crate::identity::{self, tilde};
use crate::policy::{ChainStep, Presence, Router, Tier};
use crate::slug::Slug;

/// `platform` in `auth status`'s report.
#[cfg(unix)]
const PLATFORM: &str = "unix";
/// `platform` in `auth status`'s report.
#[cfg(not(unix))]
const PLATFORM: &str = "windows: anonymous only (file credentials need unix permission checks)";

fn needs_terminal(ctx: &Ctx, verb: &str) -> Result<()> {
    if ctx.stderr_is_terminal {
        return Ok(());
    }
    Err(Error::refused(format!(
        "provisioning a credential is a person's step: agh auth {verb} needs a terminal"
    ))
    .with_next("a person runs it at a terminal"))
}

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

/// `agh auth add NAME (--file PATH | --stdin) --tier T [--owner O]… [--write [--drafts]]`.
///
/// # Errors
///
/// Refused without a terminal; usage for its arguments; `provision::add`'s errors.
pub(crate) fn auth_add(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    needs_terminal(ctx, "add")?;
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
    let gh = ctx.github()?;
    let (added, meter) = provision::add_metered(&gh, &dirs, &req, &mut *ctx.stdin)?;
    let text = |s: &str| Value::from(ctx.redactor.apply(s));
    let value = json!({
        "fingerprint": added.fingerprint.as_str(),
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

/// `agh auth remove NAME`.
///
/// # Errors
///
/// Refused without a terminal; `provision::remove`'s errors.
pub(crate) fn auth_remove(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    needs_terminal(ctx, "remove")?;
    let name = inv.require(0, "NAME")?;
    let Some(dirs) = ctx.dirs.clone() else {
        return Err(Error::could_not_run(
            "no config directory: agh cannot provision here",
        ));
    };
    let removed = provision::remove(&dirs, name)?;
    let value = json!({
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

/// `agh auth status`: profile status, ambient tokens, and `agh.toml` faults.
///
/// # Errors
///
/// None of its own: an invalid agh.toml (exit 1) and an unusable referenced profile (exit 4)
/// are reports on stdout with their error.
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
    let mut unusable: Option<(String, &'static str, Option<String>)> = None;
    match &read {
        None => {}
        Some(Err(found)) => faults.clone_from(found),
        Some(Ok(validated)) => {
            let router = Router::from_validated(validated.clone());
            let mut named = router.profiles();
            named.sort_by(|a, b| a.name.cmp(&b.name));
            for profile in named {
                profiles.push(profile_row(profile, &*ctx.env, &home, &ctx.redactor));
            }
            unusable = referenced_unusable(validated, &*ctx.env, &home);
        }
    }
    let ambient = ambient_rows(&*ctx.env, &profiles);
    let listed: Vec<Value> = faults
        .iter()
        .map(|fault| json!({"rule": fault.rule, "what": ctx.redactor.apply(&fault.what)}))
        .collect();
    let value = json!({
        "ambient": ambient,
        "config": shown,
        "faults": listed,
        "ok": faults.is_empty(),
        "platform": PLATFORM,
        "present": present,
        "profiles": profiles,
    });
    let receipt = Receipt::new(&inv.verb_name());
    let answer = if !faults.is_empty() {
        let error = crate::config::invalid(
            format!("agh.toml breaks {} rule(s)", faults.len()),
            crate::config::POLICY_CHECK,
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
