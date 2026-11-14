// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `atpkg-publish hosts check` (§16.2): whether this builder's agh.toml is ready for
//! `--host agh-staging` or `--host agh`, read-only.
//!
//! It reads explicit configuration, recorded vault identities, and the trust checkout's origin
//! and the seed ledger its origin/main carries, and sends nothing. A writer whose token is not
//! in a token file fails its check: a lane unit carries no token variable, and agh's git
//! credential helper reads only token files, so a unit reads what this check read. `--lane linux` additionally reads and authenticates the fresh signed
//! channel to select the repositories that lane may serve; the generic inventory sends nothing.

use agh::config::Dirs;
use agh::policy::Tier;

use crate::artifact::lineage_switch::{self, Switch};
use crate::ctx::{Ctx, HostChoice};
use crate::error::{Error, Result};
use crate::host::agh::{AghHost, CALLER};
use crate::lane::install::Machine;
use crate::lane::{LaneKind, linux};

/// The runbook every provisioning step points at.
const RUNBOOK: &str = "docs/RUNBOOK-atpkg-republish.md";

/// What the other four checks say when `config` fails.
const NEEDS_CONFIG: &str = "needs valid configured identity routing";

/// One check's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCheck {
    /// `config`, `staging_writer`, `public_writer`, `grants`, `drafts` or `trust_origin`; for
    /// the tip lane also `program_grants`.
    pub name: &'static str,
    /// Whether it holds.
    pub ok: bool,
    /// What was found, or what is missing.
    pub detail: String,
}

impl HostCheck {
    fn new(name: &'static str, ok: bool, detail: impl Into<String>) -> HostCheck {
        HostCheck {
            name,
            ok,
            detail: detail.into(),
        }
    }
}

/// The distinct `repo` of the lane-tracked rows of [`crate::programs::PROGRAMS`]
/// ([`crate::programs::ProgramDef::lane_tracked`]), in table order: besides the index
/// repository, the repositories the alab lane clones, stages to and publishes to, and so
/// preflights before it claims a build (`lane::alab`). A repository only package or bottle rows
/// name is not among them: those builds enter the channel through an explicit pin, not through
/// this lane.
#[must_use]
pub fn alab_program_repos() -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    for p in crate::programs::PROGRAMS {
        if p.lane_tracked() && !repos.iter().any(|r| r == p.repo) {
            repos.push(p.repo.to_string());
        }
    }
    repos
}

/// The repositories of the programs the tip lane and its ready dispatcher may carry
/// ([`crate::lane::ready::queueable`]), in table order.
#[must_use]
pub fn tip_program_repos() -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    for p in crate::programs::PROGRAMS {
        if crate::lane::ready::queueable(p.name).is_ok() && !repos.iter().any(|r| r == p.repo) {
            repos.push(p.repo.to_string());
        }
    }
    repos
}

/// [`check`] for the tip lane. Which of its programs may publish is the lifecycle registry's
/// to say at each run, and this check sends no request, so it cannot know a repository whose
/// public tier is closed from one that is open. It therefore requires the public owner's grant
/// on the index repository alone, and reports the program repositories that have none in the
/// passing row `program_grants`: a queued build of one is refused at publish and set aside by
/// the dispatcher, and a grant is only ever the answer for a repository the registry holds
/// active with an open public tier.
#[must_use]
pub fn check_tip(ctx: &Ctx, dirs: &agh::config::Dirs, choice: HostChoice) -> Vec<HostCheck> {
    let programs = tip_program_repos();
    let mut checks = check_scoped(ctx, dirs, choice, false, &programs, Some(&[]));
    let Ok(client) = config(dirs) else {
        return checks;
    };
    let public = ctx.accounts.public.as_str();
    let written = choice == HostChoice::Agh && client.router().tier_of(public) == Tier::Release;
    let ungranted: Vec<String> = programs
        .iter()
        .filter(|_| written)
        .map(|repo| format!("{public}/{repo}"))
        .filter(|slug| !granted(&client, slug))
        .collect();
    checks.push(HostCheck::new(
        "program_grants",
        true,
        if ungranted.is_empty() {
            format!("{CALLER} may write every program repository the tip lane can queue")
        } else {
            format!(
                "no grant on: {}. A queued build of one is refused at publish and set aside. \
                 Grant a repository only when the lifecycle registry holds it active with an \
                 open public tier; one it closes or holds needs none and gets none",
                ungranted.join(", ")
            )
        },
    ));
    checks
}

fn granted(client: &agh::Github, slug: &str) -> bool {
    slug.split_once('/').is_some_and(|(owner, repo)| {
        agh::Slug::new(owner, repo).is_ok_and(|s| client.router().grant(CALLER, &s).is_ok())
    })
}

/// Repositories selected by the Linux lane's signed channel pins, across its default targets.
/// Build numbers may differ by target: grants name repositories, not package versions.
fn linux_program_repos(index: &atpkg::Index) -> Result<Vec<String>> {
    let channel = index.channels.first().ok_or_else(|| {
        Error::refused("the signed index carries no channel — no Linux pins to serve")
    })?;
    let mut repos = Vec::new();
    for target in linux::default_targets() {
        for name in crate::pins::for_targets(channel, &[target])?.keys() {
            let definition = crate::programs::get(name);
            if matches!(
                linux::Class::of(name, definition),
                linux::Class::Vendor | linux::Class::AppleOnly
            ) {
                continue;
            }
            let repo = index
                .programs
                .get(name)
                .map(|p| p.repo.as_str())
                .or_else(|| definition.map(|d| d.repo))
                .unwrap_or(name);
            if !repos.iter().any(|r| r == repo) {
                repos.push(repo.to_string());
            }
        }
    }
    Ok(repos)
}

/// Judge a Linux lane against a fresh authenticated channel selection.
///
/// Configuration, writers and the index grant must hold before any request. The signed
/// channel then selects the repository grants, using the same pin and program classification
/// owners as the lane. An unselected table entry is not publication authority.
///
/// # Errors
/// What fetching and verifying the fresh signed head answers, or a channel with no pins. When
/// `trust_origin` failed, the head's error carries that row's detail after its own.
pub fn check_linux(
    ctx: &Ctx,
    dirs: &Dirs,
    choice: HostChoice,
    keyless: bool,
) -> Result<Vec<HostCheck>> {
    let preliminary = check(ctx, dirs, choice, keyless, &[]);
    // The trust checkout is local: it does not decide whether the head can be read, and the
    // grants are judged whatever it says. A head that cannot be read still names a failed
    // trust origin, so neither fault hides the other.
    if failed(&preliminary)
        .iter()
        .any(|name| *name != TRUST_ORIGIN)
    {
        return Ok(preliminary);
    }
    let head = crate::channel::Scratch::new("linux-hosts-check").and_then(|scratch| {
        crate::quad::fetch_head(ctx, &ctx.index_slug(), scratch.path()).map(|(_, head)| head)
    });
    let origin = preliminary.iter().find(|c| c.name == TRUST_ORIGIN && !c.ok);
    let head = head.map_err(|error| match origin {
        Some(row) => error.with_note(&format!("{TRUST_ORIGIN} failed too: {}", row.detail)),
        None => error,
    })?;
    Ok(check(
        ctx,
        dirs,
        choice,
        keyless,
        &linux_program_repos(&head.index)?,
    ))
}

/// The name of the row [`trust_origin`] judges.
const TRUST_ORIGIN: &str = "trust_origin";

/// The `trust_origin` row: whether the trust checkout a pack reads its lineage switch from
/// ([`lineage_switch::default_trust_repo`]) lets that switch read what the trust repository's
/// main says. [`lineage_switch::read`] reads the checkout alone first
/// ([`lineage_switch::read_locally`]): a minted ledger at its `refs/remotes/origin/main` reads
/// live, and no checkout, no origin/main or no readable ledger there reads unknown, whatever
/// its origin names, so the row holds, naming an origin that would not serve. Only a dormant
/// ledger there leaves the switch to the trust repository's live main, which under agh is read
/// through `git::main_of_record`. That refuses a checkout not admitted for local reads, or whose
/// `origin` does not name the staging owner's `trust` repository, and the switch then reads
/// unknown, never dormant, which a pack holds to the required lineage policy
/// (`Switch::policy`): the row fails exactly there. It reads local Git only and needs no
/// agh.toml, so it is judged whatever `config` says.
fn trust_origin(ctx: &Ctx) -> HostCheck {
    let repo = lineage_switch::default_trust_repo(ctx);
    let shown = ctx.shown(&repo);
    let slug = ctx.staging_slug("trust");
    let why = match crate::git::source_with_origin(&repo, &slug) {
        Ok(_) => {
            return HostCheck::new(TRUST_ORIGIN, true, format!("{shown}'s origin names {slug}"));
        }
        Err(why) => why,
    };
    match lineage_switch::read_locally(ctx, &repo) {
        lineage_switch::Local::Settled(switch) => {
            let reads = match switch {
                Switch::Live => "live: its origin/main carries a minted re-pin".to_string(),
                Switch::Unknown(unknown) => {
                    format!("unknown, held to the required lineage policy: {unknown}")
                }
                Switch::Dormant => "dormant".to_string(),
            };
            HostCheck::new(
                TRUST_ORIGIN,
                true,
                format!(
                    "{shown} would not serve as {slug}'s checkout ({why}), and that decides \
                     nothing: the checkout alone reads the lineage switch {reads}"
                ),
            )
        }
        lineage_switch::Local::Deferred(here) => HostCheck::new(
            TRUST_ORIGIN,
            false,
            format!(
                "{shown} cannot serve as {slug}'s checkout: {why}. Its origin/main ({}) carries \
                 a dormant ledger, so the lineage switch reads {slug}'s main through that origin \
                 ({RUNBOOK})",
                &here[..here.len().min(10)]
            ),
        ),
    }
}

/// The router agh.toml under `dirs` builds, or the failing `config` detail.
fn config(dirs: &Dirs) -> std::result::Result<agh::Github, String> {
    AghHost::configured_client(dirs, false).map_err(|refusal| refusal.why().to_string())
}

/// Whether `owner`'s writer is present: `Ok(profile)` names it, `Err` says why not. `slot` is
/// the vault token that writes `owner` (dev for the staging owner, release for the public one):
/// its step is the one a missing writer takes.
fn writer(
    client: &agh::Github,
    owner: &str,
    slot: agh::identity::vault::Slot,
) -> std::result::Result<String, String> {
    let router = client.router();
    let Some(name) = router.writer_of(owner) else {
        return Err(format!(
            "no profile writes {owner}: {} ({RUNBOOK})",
            slot.sync_step()
        ));
    };
    // A lane unit carries no token variable, and agh's git runner refuses a credentialed run
    // whose profile keeps its token anywhere but a token file, so only a file serves a lane.
    let profile = router.profile(name);
    if let Some(profile) = profile
        && !matches!(profile.source, agh::identity::Source::File(_))
    {
        return Err(format!(
            "{name} writes {owner}, and its token is not in a token file: a lane unit carries \
             no token variable and git's credential helper reads only token files; {} \
             ({RUNBOOK})",
            slot.sync_step()
        ));
    }
    match client.presence(name) {
        agh::policy::Presence::Present => Ok(name.to_string()),
        other => Err(format!(
            "{name} writes {owner}, and its token is {}: {}",
            other.slug(),
            profile
                .and_then(agh::identity::vault::absent_step)
                .unwrap_or_else(|| slot.sync_step())
        )),
    }
}

/// Whether a lane whose `--keyless` is `keyless` stages only, and so never writes the public
/// owner: `--keyless` lets a builder with no machine key install, and the Linux lane's run is
/// keyed whenever `ctx.identity.key` is a file, whatever its unit's flags said.
pub(crate) fn stages_only(ctx: &Ctx, keyless: bool) -> bool {
    keyless && !ctx.identity.key.is_file()
}

/// Every check, for the environment a lane unit runs with (`Dirs::resolve(home,
/// xdg_config_home, xdg_state_home)`), for `choice`. `keyless` judges the staging owner
/// alone, for a Linux lane that stages only (`stages_only`) and never writes the public
/// owner.
#[must_use]
pub fn check(
    ctx: &Ctx,
    dirs: &agh::config::Dirs,
    choice: HostChoice,
    keyless: bool,
    programs: &[String],
) -> Vec<HostCheck> {
    check_scoped(ctx, dirs, choice, keyless, programs, None)
}

/// [`check`], with the program repositories whose grant the public owner must carry given
/// apart (`None`: the same `programs` as every other owner).
fn check_scoped(
    ctx: &Ctx,
    dirs: &agh::config::Dirs,
    choice: HostChoice,
    keyless: bool,
    programs: &[String],
    public_programs: Option<&[String]>,
) -> Vec<HostCheck> {
    let client = match config(dirs) {
        Ok(client) => client,
        Err(detail) => {
            let mut checks = vec![HostCheck::new("config", false, detail)];
            for name in ["staging_writer", "public_writer", "grants", "drafts"] {
                checks.push(HostCheck::new(name, false, NEEDS_CONFIG));
            }
            checks.push(trust_origin(ctx));
            return checks;
        }
    };
    let h = choice.as_str();
    let mut checks = vec![HostCheck::new(
        "config",
        true,
        "configured identities resolved from files and recorded vault state; no requests sent",
    )];
    let (staging, public) = (ctx.accounts.staging.as_str(), ctx.accounts.public.as_str());
    let mut present: Vec<String> = Vec::new();
    let mut judge = |name: &'static str, owner: &str, slot| match writer(&client, owner, slot) {
        Ok(profile) => {
            let detail = format!("{profile} writes {owner}");
            if !present.contains(&profile) {
                present.push(profile);
            }
            HostCheck::new(name, true, detail)
        }
        Err(why) => HostCheck::new(name, false, why),
    };
    checks.push(judge(
        "staging_writer",
        staging,
        agh::identity::vault::Slot::Dev,
    ));
    let public_written = choice == HostChoice::Agh && !keyless;
    checks.push(if public_written {
        judge("public_writer", public, agh::identity::vault::Slot::Release)
    } else if keyless {
        HostCheck::new("public_writer", true, "not needed by a keyless lane")
    } else {
        HostCheck::new(
            "public_writer",
            true,
            format!("not needed under --host {h}"),
        )
    });

    // The grants: every release-tier owner agh writes under this choice, each repository the
    // lanes write or clone there (findings F3 and F6).
    let mut owners: Vec<&str> = vec![staging];
    if public_written && !owners.iter().any(|o| o.eq_ignore_ascii_case(public)) {
        owners.push(public);
    }
    owners.retain(|o| client.router().tier_of(o) == Tier::Release);
    let mut slugs: Vec<String> = Vec::new();
    for owner in &owners {
        let programs = match public_programs {
            Some(narrowed) if owner.eq_ignore_ascii_case(public) => narrowed,
            _ => programs,
        };
        let repos =
            std::iter::once(atpkg::manifest::INDEX_REPO).chain(programs.iter().map(String::as_str));
        for repo in repos {
            let slug = format!("{owner}/{repo}");
            if !slugs.contains(&slug) {
                slugs.push(slug);
            }
        }
    }
    let missing: Vec<&str> = slugs
        .iter()
        .filter(|slug| !granted(&client, slug))
        .map(String::as_str)
        .collect();
    checks.push(if !missing.is_empty() {
        HostCheck::new(
            "grants",
            false,
            format!(
                "missing: {}; run agh policy grant {CALLER} REPO for each",
                missing.join(", ")
            ),
        )
    } else if owners.is_empty() {
        HostCheck::new(
            "grants",
            true,
            format!("no release-tier owner is written through agh under --host {h}"),
        )
    } else {
        HostCheck::new(
            "grants",
            true,
            format!(
                "{CALLER} may write {} repositories of {}",
                slugs.len(),
                owners.join(", ")
            ),
        )
    });

    // Drafts (design §6.4; the fail-closed gate stays).
    let mut unproven: Vec<String> = Vec::new();
    for name in &present {
        if !client.drafts_vouched(name) {
            unproven.push(format!(
                "{name} is fine-grained and not vouched for drafts: re-add it from its token \
                 file with --drafts"
            ));
        }
    }
    checks.push(if unproven.is_empty() {
        HostCheck::new("drafts", true, "every writer may list drafts")
    } else {
        HostCheck::new("drafts", false, unproven.join("; "))
    });
    checks.push(trust_origin(ctx));
    checks
}

/// The names of the checks that failed, in order: what `hosts check` and `lane install --host
/// agh*` each refuse with.
pub(crate) fn failed(checks: &[HostCheck]) -> Vec<&'static str> {
    checks.iter().filter(|c| !c.ok).map(|c| c.name).collect()
}

/// One object, its keys sorted: `{"checks":[{"detail":…,"name":…,"ok":…},…],"host":…,"ok":…}`.
pub(crate) fn render(choice: HostChoice, checks: &[HostCheck]) -> String {
    let rows: Vec<aterm_json::Value> = checks
        .iter()
        .map(|c| {
            aterm_json::json!({
                "detail": c.detail.as_str(),
                "name": c.name,
                "ok": c.ok,
            })
        })
        .collect();
    let all = checks.iter().all(|c| c.ok);
    let object = aterm_json::json!({
        "checks": rows,
        "host": choice.as_str(),
        "ok": all,
    });
    object.to_string()
}

/// The CLI verb: `atpkg-publish hosts check [--host agh-staging|agh] [--unit-env] [--keyless]
/// [--lane alab|linux]`. Linux scope authenticates the fresh signed channel; without a lane,
/// this is the local check of the alab lane's repositories ([`alab_program_repos`]).
/// It prints one JSON object, `{"checks":[…],"host":"…","ok":…}`, and exits 0 when every
/// check holds, else 1 (Refused). `--keyless` judges a staging-only lane only on a builder
/// with no machine key: a keyed builder's run writes the public owner, so it is judged keyed.
///
/// # Errors
///
/// Usage for a bad flag. With `--unit-env`, `Machine::production`'s refusal (an OS other than
/// macOS or Linux). For Linux scope, the fresh signed-head check can also refuse. Could-not-run
/// when agh's directories cannot be resolved. Refused, after
/// the object is printed, when any check fails.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let CheckFlags {
        choice,
        unit_env,
        keyless,
        lane,
    } = parse(args)?;
    let dirs = if unit_env {
        Machine::production(ctx)?.agh_dirs(ctx)
    } else {
        Dirs::from_env()
    }
    .ok_or_else(|| {
        Error::cannot_run(
            "agh's directories cannot be resolved: no home, config or state directory",
        )
    })?;
    let checks = if lane == Some(LaneKind::Linux) {
        check_linux(ctx, &dirs, choice, stages_only(ctx, keyless))?
    } else if lane == Some(LaneKind::Tip) {
        check_tip(ctx, &dirs, choice)
    } else {
        check(
            ctx,
            &dirs,
            choice,
            stages_only(ctx, keyless),
            &alab_program_repos(),
        )
    };
    println!("{}", render(choice, &checks));
    let failed = failed(&checks);
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "hosts check --host {}: {} failed",
            choice.as_str(),
            failed.join(", ")
        )))
    }
}

struct CheckFlags {
    choice: HostChoice,
    unit_env: bool,
    keyless: bool,
    lane: Option<LaneKind>,
}

/// Linux metadata uses the parsed transport; a generic local inventory needs no host context.
pub(crate) fn context_host(args: &[String]) -> Result<HostChoice> {
    let flags = parse(args)?;
    Ok(if flags.lane == Some(LaneKind::Linux) {
        flags.choice
    } else {
        HostChoice::Gh
    })
}

fn parse(args: &[String]) -> Result<CheckFlags> {
    let Some(("check", flags)) = args.split_first().map(|(v, r)| (v.as_str(), r)) else {
        return Err(Error::usage("hosts takes one sub-verb: check"));
    };
    let mut choice = HostChoice::Agh;
    let (mut unit_env, mut keyless) = (false, false);
    let mut lane = None;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = match flag.as_str() {
            "--unit-env" => {
                unit_env = true;
                continue;
            }
            "--keyless" => {
                keyless = true;
                continue;
            }
            "--lane" => {
                let value = it
                    .next()
                    .ok_or_else(|| Error::usage("--lane needs alab, linux or tip"))?;
                lane = Some(LaneKind::parse(value)?);
                continue;
            }
            other => match HostChoice::flag_value(other, &mut it) {
                Some(value) => value,
                None => {
                    return Err(Error::usage(format!(
                        "hosts check does not take `{other}` (--host agh-staging|agh, \
                         --unit-env, --keyless, --lane alab|linux)"
                    )));
                }
            },
        };
        choice = match HostChoice::parse(value) {
            Some(c @ (HostChoice::AghStaging | HostChoice::Agh)) => c,
            _ => {
                return Err(Error::usage(format!(
                    "hosts check --host takes agh-staging or agh: {value}"
                )));
            }
        };
    }
    Ok(CheckFlags {
        choice,
        unit_env,
        keyless,
        lane,
    })
}

#[cfg(test)]
#[path = "hosts_check_tests.rs"]
mod tests;
