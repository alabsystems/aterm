// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh whoami`: the identities agh knows, or how a request would be routed. The identity rows
//! are shared with `agh auth status`.

use std::path::Path;

use aterm_json::{Value, json};

use super::args::Invocation;
use super::cmd_credential::helper_dirs;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::{Kind, Result};
use crate::git::credential::{self, Asked, CredentialAnswer, CredentialRequest};
use crate::git::grant::LiveGrants;
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
    fn field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
        row.get(key).and_then(Value::as_str)
    }
    identity::ambient(env)
        .into_iter()
        .map(|ambient| {
            let source = format!("env:{}", ambient.var);
            let by_source = profiles
                .iter()
                .find(|row| field(row, "source") == Some(source.as_str()))
                .and_then(|row| field(row, "profile"));
            let by_token = profiles
                .iter()
                .find(|row| field(row, "fingerprint") == Some(ambient.fingerprint.as_str()))
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

/// `agh whoami [--for REPO [--op read|writer-read|write]] [--verify] [--git URL]`.
///
/// # Errors
///
/// Usage for the flags' combinations (`--git` with `--for`, `--op`, `--verify`, `--as` or
/// `--anonymous`, none of which git's helper sees); the client's refusal of an invalid
/// agh.toml; with
/// `--verify`, any error of `GET /user` but a 401; with `--git`, Usage for a URL that is not
/// a github.com repository's.
pub(crate) fn whoami(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    if let Some(url) = inv.flag("--git") {
        if inv.has("--for")
            || inv.has("--op")
            || inv.has("--verify")
            || inv.global.anonymous
            || inv.global.as_profile.is_some()
        {
            return Err(inv.usage(
                "--git takes no --for, --op, --verify, --as or --anonymous: git's credential \
                 helper never sees them",
            ));
        }
        return whoami_git(ctx, inv, url);
    }
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
    profiles.sort_unstable_by(|a, b| a.name.cmp(&b.name));
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
        rows.sort_unstable_by(|a, b| {
            a.get("profile")
                .and_then(Value::as_str)
                .cmp(&b.get("profile").and_then(Value::as_str))
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

/// The `get` request git would send for `url`, and the loopback host a runner pointing git
/// there would name: `https://github.com/{owner}/{repo}[.git]`, or, in a test or debug build,
/// `http://127.0.0.1:{port}/{owner}/{repo}[.git]`, with the repository it names. `None` for
/// anything else.
fn git_request(url: &str) -> Option<(CredentialRequest, crate::slug::Slug, Option<String>)> {
    let (protocol, host, path, loopback) =
        if let Some(path) = url.strip_prefix("https://github.com/") {
            ("https", "github.com".to_string(), path, None)
        } else {
            loopback_request(url)?
        };
    let req = CredentialRequest {
        op: "get".to_string(),
        protocol: Some(protocol.to_string()),
        host: Some(host),
        path: Some(path.to_string()),
    };
    let slug = req.slug()?;
    Some((req, slug, loopback))
}

/// `http://127.0.0.1:{port}/{path}` in a test or debug build: `http`, its host, its path and
/// that host as the loopback a runner would name.
#[cfg(any(test, debug_assertions))]
fn loopback_request(url: &str) -> Option<(&'static str, String, &str, Option<String>)> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = rest.split_once('/')?;
    crate::path::digits::<u16>(port)?;
    let host = format!("127.0.0.1:{port}");
    Some(("http", host.clone(), path, Some(host)))
}

/// A release build answers only github.com.
#[cfg(not(any(test, debug_assertions)))]
fn loopback_request(url: &str) -> Option<(&'static str, String, &str, Option<String>)> {
    let _ = url;
    None
}

/// `agh whoami --git URL`: the identity the git helper would answer for `url`, decided as the
/// helper decides (its directories, its route with vault adoption, and the live grants), with
/// no token loaded past presence and none printed. Where the helper quits silently on an
/// invalid agh.toml or adoption's refusal, this verb fails with that refusal, as every routed
/// verb does.
fn whoami_git(ctx: &mut Ctx, inv: &Invocation, url: &str) -> Result<Outcome> {
    let Some((req, slug, loopback)) = git_request(url) else {
        return Err(inv.usage(
            "--git wants https://github.com/OWNER/REPO or https://github.com/OWNER/REPO.git",
        ));
    };
    let now = ctx.fx.wall_secs()?;
    let env = &*ctx.env;
    let nonce = credential::var(env, crate::git::GRANT_VAR);
    let asked = Asked {
        stdin_is_terminal: false,
        nonce: nonce.as_deref(),
        loopback: loopback.as_deref(),
        now,
    };
    let (answer, granted, router) = match helper_dirs(env, ctx.dirs.as_ref()) {
        Some(dirs) => {
            let router = credential::helper_route(env, &dirs)?.router;
            let presence = |name: &str| router.presence(name, env);
            let (answer, granted) =
                credential::explained(&req, &asked, &router, &LiveGrants::new(&dirs), &presence);
            (answer, granted, router)
        }
        None => (
            CredentialAnswer::Quit {
                why: "agh has no config directory, so the helper answers nothing".to_string(),
            },
            false,
            crate::policy::Router::empty(),
        ),
    };
    let text = |s: &str| Value::from(ctx.redactor.apply(s));
    let value = match &answer {
        CredentialAnswer::Credential { profile, tier } => json!({
            "answer": "credential",
            "grant": if granted { Value::from("live") } else { Value::Null },
            "profile": profile.as_str(),
            "repo": slug.to_string(),
            "tier": tier.slug(),
            "why": null,
        }),
        CredentialAnswer::Quit { why } => json!({
            "answer": "quit",
            "grant": null,
            "profile": null,
            "repo": slug.to_string(),
            "tier": router.tier_of(slug.owner()).slug(),
            "why": text(why),
        }),
    };
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Credential,
            value,
        },
        receipt: Receipt::new(&inv.verb_name()),
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
