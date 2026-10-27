// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh whoami`: the identities agh knows, or how a request would be routed. The identity rows
//! are shared with `agh auth status`.

use std::path::Path;

use aterm_json::{Value, json};

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::{Kind, Result};
use crate::identity::vault::{Adoption, Found};
use crate::identity::{self, EnvSource, Profile, Source, tilde};
use crate::policy::{Access, Presence, RouteRequest, RouteTarget};
use crate::redact::Redactor;

/// A presence as one word: `present`, `absent`, or `unusable`.
const fn presence_word(presence: Presence) -> &'static str {
    match presence {
        Presence::Present => "present",
        Presence::Absent => "absent",
        Presence::Unusable(_) => "unusable",
    }
}

/// `file:{path, ~ for home}` or `env:{VAR}`, the variable's name as `config::masked` shows
/// it: a token pasted into `env` is never printed whole, though L1 refuses one.
fn source_text(source: &Source, home: &Path) -> String {
    match source {
        Source::File(path) => format!("file:{}", tilde(path, home)),
        Source::Env(var) => format!("env:{}", crate::config::masked(var)),
        Source::Inline(_) => "inline".to_string(),
    }
}

/// Where a profile row came from: `agh.toml`, or `vault` (adopted, design §6.10).
pub(super) const FROM_CONFIG: &str = "agh.toml";
/// Where an adopted profile's row came from.
pub(super) const FROM_VAULT: &str = "vault";
/// What routes when an agh.toml that says `vault = true` joins the adopted rows.
pub(super) const FROM_JOINED: &str = "agh.toml+vault";

/// One profile's row, its credential loaded under the strict rules (and dropped); `from` says
/// where the profile came from.
pub(super) fn profile_row(
    profile: &Profile,
    env: &dyn EnvSource,
    home: &Path,
    redactor: &Redactor,
    from: &str,
) -> Value {
    let text = |s: &str| Value::from(redactor.apply(s));
    // A login is agh.toml text no name rule holds, so a token pasted there is masked too.
    let login = profile
        .login
        .as_deref()
        .map_or(Value::Null, |l| text(&crate::config::masked(l)));
    let source = text(&source_text(&profile.source, home));
    match identity::load(profile, env) {
        Ok(cred) => json!({
            "fingerprint": cred.fingerprint(),
            "from": from,
            "kind": cred.kind().slug(),
            "login": login,
            "presence": "present",
            "profile": profile.name.as_str(),
            "remedy": null,
            "source": source,
            "tier": profile.tier.slug(),
            "why": null,
        }),
        Err(e) => json!({
            "fingerprint": null,
            "from": from,
            "kind": null,
            "login": login,
            "presence": presence_word(e.presence()),
            "profile": profile.name.as_str(),
            "remedy": e.remedy(profile, home).map_or(Value::Null, |r| text(&r)),
            "source": source,
            "tier": profile.tier.slug(),
            "why": text(&format!("{}: {e}", e.slug())),
        }),
    }
}

/// One row per ambient token (`GH_TOKEN`, then `GITHUB_TOKEN`), never used as ambient. Its
/// remedy names the matching profile in `profiles` ([`profile_row`]).
pub(super) fn ambient_rows(env: &dyn EnvSource, profiles: &[Value]) -> Vec<Value> {
    let field = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_string);
    identity::ambient(env)
        .into_iter()
        .map(|ambient| {
            let source = format!("env:{}", ambient.var);
            let by_source = profiles
                .iter()
                .find(|row| field(row, "source").as_deref() == Some(source.as_str()))
                .and_then(|row| field(row, "profile"));
            let by_token = profiles
                .iter()
                .find(|row| {
                    field(row, "fingerprint").as_deref() == Some(ambient.fingerprint.as_str())
                })
                .and_then(|row| field(row, "profile"));
            let remedy = match (by_source, by_token) {
                (Some(profile), _) => {
                    format!("used by profile {profile} (env = \"{}\")", ambient.var)
                }
                (None, Some(profile)) => format!(
                    "never used: it is profile {profile}'s token, which agh reads from that \
                     profile's source"
                ),
                (None, None) => format!(
                    "never used: name it with env = \"{}\" in a dev or external profile to use it",
                    ambient.var
                ),
            };
            json!({
                "fingerprint": ambient.fingerprint.as_str(),
                "from": "env",
                "kind": ambient.kind.map(|k| k.slug()),
                "login": null,
                "presence": "present",
                "profile": null,
                "remedy": remedy,
                "source": format!("env:{}", ambient.var),
                "tier": null,
                "why": null,
            })
        })
        .collect()
}

/// One row per vault token adoption found (design §6.10), `dev` then `release`: an adopted one
/// as its profile's row, and one that is not adopted with why and what to do. A token with no
/// file has no row.
pub(super) fn vault_rows(
    adoption: &Adoption,
    env: &dyn EnvSource,
    home: &Path,
    redactor: &Redactor,
) -> Vec<Value> {
    let text = |s: &str| Value::from(redactor.apply(s));
    adoption
        .vaulted()
        .iter()
        .filter_map(|vaulted| {
            let slot = vaulted.slot;
            let source = text(&format!("file:{}", tilde(&vaulted.token_file, home)));
            let row = |fingerprint: Value, kind: Value, login: Value, presence: &str| {
                json!({
                    "fingerprint": fingerprint,
                    "from": FROM_VAULT,
                    "kind": kind,
                    "login": login,
                    "presence": presence,
                    "profile": slot.profile(),
                    "remedy": null,
                    "source": source.clone(),
                    "tier": slot.tier().slug(),
                    "why": null,
                })
            };
            let with = |mut row: Value, why: String, remedy: Option<String>| {
                if let Some(fields) = row.as_object_mut() {
                    fields.insert("why".into(), text(&why));
                    fields.insert("remedy".into(), remedy.map_or(Value::Null, |r| text(&r)));
                }
                row
            };
            match &vaulted.found {
                Found::Absent => None,
                Found::Adopted { .. } => adoption
                    .profile(slot.profile())
                    .map(|profile| profile_row(profile, env, home, redactor, FROM_VAULT)),
                Found::Unusable { slug, why, remedy } => Some(with(
                    row(Value::Null, Value::Null, Value::Null, "unusable"),
                    format!("{slug}: {why}"),
                    remedy.clone(),
                )),
                Found::Faulted {
                    login,
                    fingerprint,
                    why,
                    next,
                } => Some(with(
                    row(
                        Value::from(fingerprint.as_str()),
                        Value::Null,
                        login.as_deref().map_or(Value::Null, text),
                        "unusable",
                    ),
                    format!("rule_fault: {why}"),
                    Some(next.clone()),
                )),
                Found::Unlearned {
                    fingerprint,
                    kind,
                    why,
                    ..
                } => Some(with(
                    row(
                        Value::from(fingerprint.as_str()),
                        Value::from(kind.slug()),
                        Value::Null,
                        "present",
                    ),
                    format!("not_adopted: {why}"),
                    vaulted.next(),
                )),
                Found::Changed {
                    fingerprint,
                    kind,
                    adopted,
                    now,
                } => Some(with(
                    row(
                        Value::from(fingerprint.as_str()),
                        Value::from(kind.slug()),
                        text(adopted),
                        "present",
                    ),
                    format!("login_changed: adopted as {adopted}, GitHub now says {now}"),
                    adoption.refusal().map(|refusal| refusal.next.clone()),
                )),
            }
        })
        .collect()
}

/// `agh whoami [--for REPO [--op read|writer-read|write]] [--verify]`.
///
/// # Errors
///
/// Usage for the flags' combinations; the client's refusal of an invalid agh.toml; with
/// `--verify`, any error of `GET /user` but a 401.
pub(crate) fn whoami(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let usage = |why: &str| inv.usage(why);
    let target = inv.flag("--for");
    if inv.has("--op") && target.is_none() {
        return Err(usage("--op needs --for REPO"));
    }
    if target.is_some() && inv.has("--verify") {
        return Err(usage("--for and --verify contradict each other"));
    }
    let access = match inv.flag("--op") {
        None => Access::Read,
        Some(op) => {
            Access::parse(op).ok_or_else(|| usage("--op wants read, writer-read or write"))?
        }
    };
    let gh = ctx.github()?;
    let receipt = Receipt::new(&inv.verb_name());
    if let Some(text) = target {
        let (slug, named) = super::args::resolve_repo(text, gh.router())?;
        let request = RouteRequest {
            target: RouteTarget::Repo(slug),
            access,
            named,
            anonymous: inv.global.anonymous,
            as_profile: inv.global.as_profile.clone(),
        };
        let explanation = gh.router().explain(&request, &|p| gh.presence(p));
        return Ok(Outcome {
            answer: Answer::Object {
                noun: Noun::Route,
                value: explanation.to_json(),
            },
            receipt,
        });
    }
    let home = ctx
        .dirs
        .as_ref()
        .map(|dirs| dirs.home.clone())
        .unwrap_or_default();
    let verifying = inv.has("--verify");
    if verifying && inv.global.anonymous {
        return Err(usage(
            "--verify sends each profile's token: drop --anonymous",
        ));
    }
    let only = inv.global.as_profile.as_deref().filter(|_| verifying);
    if let Some(name) = only
        && gh.router().profile(name).is_none()
    {
        return Err(crate::config::no_profile(name, "agh whoami"));
    }
    let adoption = ctx.adoption.clone();
    let mut profiles = gh.router().profiles();
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    let mut rows = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let from = if adoption.as_ref().is_some_and(|a| a.adopted(&profile.name)) {
            FROM_VAULT
        } else {
            FROM_CONFIG
        };
        let mut row = profile_row(profile, &*ctx.env, &home, &ctx.redactor, from);
        let present = row.get("presence").and_then(Value::as_str) == Some("present");
        let chosen = only.is_none_or(|name| name == profile.name);
        if verifying && present && chosen {
            verify(&gh, profile, &mut row, &ctx.redactor)?;
        }
        rows.push(row);
    }
    if let Some(adoption) = &adoption {
        let routed: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                row.get("profile")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        rows.extend(
            vault_rows(adoption, &*ctx.env, &home, &ctx.redactor)
                .into_iter()
                .filter(|row| {
                    row.get("profile")
                        .and_then(Value::as_str)
                        .is_some_and(|name| !routed.iter().any(|r| r == name))
                }),
        );
        rows.sort_by(|a, b| {
            let name = |row: &Value| {
                row.get("profile")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            name(a).cmp(&name(b))
        });
    }
    let ambient = ambient_rows(&*ctx.env, &rows);
    rows.extend(ambient);
    let receipt = receipt.metered(&gh.meter());
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::Identity,
            items: rows,
            completeness: Completeness::Complete,
        },
        receipt,
    })
}

/// `--verify`: `GET /user` as the profile. The bound login leaves the row as it is; another
/// login is `login_mismatch`; a 401 makes the row unusable.
fn verify(
    gh: &crate::client::Github,
    profile: &Profile,
    row: &mut Value,
    redactor: &Redactor,
) -> Result<()> {
    let Some(fields) = row.as_object_mut() else {
        return Ok(());
    };
    match gh.users().me(&profile.name) {
        Ok(user) => {
            // A profile that binds no login (a read profile, L8) has nothing to disagree with.
            if let Some(bound) = profile.login.as_deref()
                && !bound.eq_ignore_ascii_case(&user.login)
            {
                let why = format!(
                    "login_mismatch: agh.toml binds {bound}, GitHub says {}",
                    user.login
                );
                fields.insert("why".into(), Value::from(redactor.apply(&why)));
            }
            Ok(())
        }
        Err(e)
            if e.kind == Kind::Unauthenticated
                && e.http.as_ref().is_some_and(|h| h.status == 401) =>
        {
            fields.insert("presence".into(), Value::from("unusable"));
            fields.insert("why".into(), Value::from("github_refused: 401"));
            Ok(())
        }
        Err(e) => Err(e),
    }
}
