// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity verbs: `auth status`, `auth add` and `auth remove`.

use std::path::Path;

use aterm_json::{Value, json};

use super::args::Invocation;
use super::cmd_whoami::{
    FROM_CONFIG, FROM_JOINED, FROM_VAULT, ambient_rows, profile_row, vault_rows,
};
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::client::Meter;
use crate::config::{ConfigFault, Dirs};
use crate::error::{Error, Kind, Result};
use crate::identity::provision::{self, AddOptions, AddRequest, Added};
use crate::identity::vault::Found;
use crate::identity::{self, tilde};
use crate::policy::{ChainStep, Presence, Tier};
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

/// `auth add`'s request.
fn add_request(inv: &Invocation) -> Result<AddRequest> {
    let usage = |why: &str| inv.usage(why);
    let name = inv.require(0, "NAME")?;
    check_name(name)?;
    if inv.global.anonymous {
        return Err(usage(&format!(
            "--anonymous sends no credential, and agh {} binds the login with the new token: drop --anonymous",
            inv.verb_name().replace('.', " ")
        )));
    }
    let tier = inv
        .flag("--tier")
        .and_then(Tier::parse)
        .ok_or_else(|| usage("--tier wants dev, release or external"))?;
    let file = token_file(inv)?;
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
        file,
        tier,
        owners,
        write,
        drafts,
    })
}

/// `auth add`'s `--file PATH`, made absolute against the current directory.
///
/// # Errors
///
/// Usage without it; could_not_run when the current directory cannot be read.
fn token_file(inv: &Invocation) -> Result<std::path::PathBuf> {
    let Some(path) = inv.flag("--file") else {
        return Err(inv.usage(
            "name the token: --file ~/.secrets/NAME names the vault entry NAME in this Mac's \
             store, or --file PATH a token file of your own",
        ));
    };
    let path = Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()
        .map_err(|e| Error::could_not_run(format!("the current directory: {e}")))?;
    Ok(cwd.join(path))
}

/// `agh auth add NAME --file PATH --tier T [--owner O]… [--write [--drafts]]
/// [--legacy-loose]`: routes the vault entry `~/.secrets/ENTRY` names (read in place from the
/// store), or a token file of the person's own at PATH, as profile NAME.
///
/// # Errors
///
/// Usage for its arguments; `provision::add_with`'s errors.
pub(crate) fn auth_add(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let req = add_request(inv)?;
    let options = AddOptions::default().legacy_loose(inv.has("--legacy-loose"));
    let dirs = provisioning_dirs(ctx)?;
    // Provisioning never adopts: a vault token adoption refuses or leaves out never stops an
    // agent from writing the agh.toml that routes past it.
    let gh = ctx.provisioning_client();
    let (added, meter) = provision::add_with_metered(&gh, &dirs, &req, options)?;
    let mut outcome = added_outcome(ctx, inv, &dirs, &req, &added, &meter);
    if options.legacy_loose {
        strict_presence(ctx, &dirs, &req, &added, &mut outcome);
    }
    Ok(outcome)
}

/// A `--legacy-loose` add's presence as the CLI judges the profile: only AghHost's loader
/// honours `legacy_loose` (design §6.6), so `agh auth status` and every other verb load the
/// file strictly. A loose file is `unusable` there, with its why and remedy, and the add's
/// answer says so rather than `present`.
fn strict_presence(ctx: &Ctx, dirs: &Dirs, req: &AddRequest, added: &Added, outcome: &mut Outcome) {
    let profile = identity::Profile::new(added.profile.as_str(), added.source.clone(), req.tier);
    let row = profile_row(&profile, &dirs.home, &ctx.redactor, FROM_CONFIG);
    if let Answer::Object { value, .. } = &mut outcome.answer
        && let Some(fields) = value.as_object_mut()
    {
        for key in ["presence", "remedy", "why"] {
            fields.insert(key.into(), row.get(key).cloned().unwrap_or(Value::Null));
        }
    }
}

/// The directories a provisioning verb writes.
///
/// # Errors
///
/// could_not_run when the platform names none.
fn provisioning_dirs(ctx: &Ctx) -> Result<Dirs> {
    ctx.dirs
        .clone()
        .ok_or_else(|| Error::could_not_run("no config directory: agh cannot provision here"))
}

/// `auth add`'s answer: the new profile as an Identity object, with the
/// receipt of the `GET /user` that bound it.
fn added_outcome(
    ctx: &Ctx,
    inv: &Invocation,
    dirs: &Dirs,
    req: &AddRequest,
    added: &Added,
    meter: &Meter,
) -> Outcome {
    let text = |s: &str| Value::from(ctx.redactor.apply(s));
    let value = json!({
        "fingerprint": added.fingerprint.as_str(),
        "from": FROM_CONFIG,
        "kind": added.kind.slug(),
        "login": text(&added.login),
        "owners": added.owners.clone(),
        "presence": "present",
        "profile": added.profile.as_str(),
        "remedy": null,
        "source": text(&identity::source_text(&added.source, &dirs.home)),
        "tier": req.tier.slug(),
        "why": null,
    });
    Outcome {
        answer: Answer::Object {
            noun: Noun::Identity,
            value,
        },
        receipt: Receipt::new(&inv.verb_name()).metered(meter),
    }
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
    let dirs = provisioning_dirs(ctx)?;
    let removed = provision::remove(&dirs, name)?;
    let value = json!({
        "joins_vault": removed.joins_vault,
        "owners_edited": removed.owners_edited,
        "profile": removed.profile.as_str(),
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
    let mut vault_next: Option<String> = None;
    let mut unusable: Option<(String, &'static str, Option<String>)> = None;
    let mut refusal = None;
    let mut routing = if present { FROM_CONFIG } else { "anonymous" };
    let joins_vault = match read {
        None => true,
        Some(Err(found)) => {
            faults = found;
            false
        }
        Some(Ok(validated)) => {
            let mut named: Vec<_> = validated.profiles().values().collect();
            named.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            for profile in named {
                profiles.push(profile_row(profile, &home, &ctx.redactor, FROM_CONFIG));
            }
            unusable = referenced_unusable(&validated, &home);
            validated.joins_vault()
        }
    };
    if joins_vault {
        let _ = ctx.adoption();
    }
    let adoption = joins_vault.then_some(ctx.adoption.as_ref()).flatten();
    if let Some(adoption) = adoption {
        profiles.extend(vault_rows(adoption, &home, &ctx.redactor));
        profiles.sort_unstable_by(|a, b| {
            a.get("profile")
                .and_then(Value::as_str)
                .cmp(&b.get("profile").and_then(Value::as_str))
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
        vault_next = adoption.next_step();
        refusal = adoption.refusal().cloned();
    }
    let vault_faults = adoption.map_or(&[][..], |a| a.faults());
    let ambient = ambient_rows(&*ctx.env, &profiles);
    let listed: Vec<Value> = faults
        .iter()
        .chain(vault_faults)
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
        let error = identity::load(profile).err()?;
        match error.presence() {
            Presence::Unusable(slug) => Some((name.to_string(), slug, error.remedy(profile, home))),
            Presence::Present | Presence::Absent => None,
        }
    })
}
