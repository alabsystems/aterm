// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth git-credential get|store|erase` (design §6.9): git's helper protocol.

use crate::error::{Error, Result};
use crate::identity::EnvSource;
use crate::policy::{Decision, Presence, RouteRequest, RouteTarget, Router, Tier};
use crate::slug::Slug;

use super::grant::GrantView;

/// The most bytes of a request the helper reads: git's requests are a few short lines.
pub(crate) const REQUEST_MAX: usize = 64 * 1024;

/// What git asked: the protocol's `key=value` lines up to a blank line or the end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CredentialRequest {
    /// `get`, `store` or `erase`.
    pub op: String,
    /// `protocol=`.
    pub protocol: Option<String>,
    /// `host=`.
    pub host: Option<String>,
    /// `path=` (present only under `credential.useHttpPath=true`).
    pub path: Option<String>,
}

/// Usage, why `git sent a malformed credential request: {what}`.
fn malformed(what: &str) -> Error {
    Error::usage(format!("git sent a malformed credential request: {what}"))
}

impl CredentialRequest {
    /// The request read from git's text. Lines past 64 KiB, a line with no `=`, or a NUL are
    /// a malformed request. A key given twice keeps its last value, as git's own reader does,
    /// and a key agh does not read is left out.
    ///
    /// # Errors
    ///
    /// Usage, why `git sent a malformed credential request: {what}`.
    pub fn parse(op: &str, text: &str) -> Result<CredentialRequest> {
        if text.len() > REQUEST_MAX {
            return Err(malformed("it is over 64 KiB"));
        }
        if text.contains('\0') {
            return Err(malformed("it holds a NUL byte"));
        }
        let mut req = CredentialRequest {
            op: op.to_string(),
            ..CredentialRequest::default()
        };
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                break;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(malformed("a line has no ="));
            };
            let slot = match key {
                "protocol" => &mut req.protocol,
                "host" => &mut req.host,
                "path" => &mut req.path,
                _ => continue,
            };
            *slot = Some(value.to_string());
        }
        Ok(req)
    }

    /// The slug the path names: `<owner>/<repo>` or `<owner>/<repo>.git`.
    #[must_use]
    pub fn slug(&self) -> Option<Slug> {
        let path = self.path.as_deref()?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(repo), None) => Slug::new(owner, repo).ok(),
            _ => None,
        }
    }
}

/// What the helper answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialAnswer {
    /// `username=x-access-token` and the profile's token.
    Credential {
        /// The profile.
        profile: String,
        /// That profile's own tier, as the router holds it (`Router::profile`), never the
        /// slug's. `project_answer` reads it.
        tier: Tier,
    },
    /// `quit=1` and nothing more: git asks no other helper and fails the request.
    Quit {
        /// Why, for `agh whoami --git` and the debug log; never printed to git.
        why: String,
    },
}

/// What the helper knows of the ask besides git's request. The caller reads each one: the
/// decision reads no environment and no clock, so the conformance drives it with a map. It is
/// one struct so that `answer` keeps to plan §1 rule 4's seven parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Asked<'a> {
    /// Whether the helper's stdin is a terminal (rule 1).
    pub(crate) stdin_is_terminal: bool,
    /// `AGH_GIT_GRANT`, as the caller read it (rules 5 and 6).
    pub(crate) nonce: Option<&'a str>,
    /// `AGH_GIT_LOOPBACK`, as the caller read it: the one `127.0.0.1:<port>` host a runner
    /// pointed this run at (rule 4).
    pub(crate) loopback: Option<&'a str>,
    /// The caller's `Fx::wall_secs`.
    pub(crate) now: u64,
}

/// `Quit` with `why`.
fn quit(why: impl Into<String>) -> CredentialAnswer {
    CredentialAnswer::Quit { why: why.into() }
}

/// The helper's decision (design §6.9). In order:
/// 1. stdin a terminal → `Quit`;
/// 2. `op` other than `get` → `Quit` (store and erase are no-ops);
/// 3. no `path`, or a `path` that names no repository → `Quit`. A path names one only when
///    `req.slug()` reads a slug from it: exactly `OWNER/REPO` or `OWNER/REPO.git`, each part
///    as `Slug::new` accepts it, so extra segments or a percent-encoded owner quit. Rules 5
///    and 6 read that slug;
/// 4. `protocol` not `https`, or `host` not `github.com` → `Quit`. The one exception holds
///    only in a test or debug build: `protocol=http` with `host` equal to `asked.loopback`.
///    A runner sets `AGH_GIT_LOOPBACK` only when its URL is a loopback endpoint, so a debug
///    build that talks to github.com never answers over plain http, and neither does a
///    redirect to another port;
/// 5. dev or external tier:
///    - the live grant `asked.nonce` names, for exactly this slug, answers its profile when
///      that profile is `router.writer_of(owner)`, is not release tier, and is Present. That
///      is the writer a runner's `PushDelete` routed;
///    - that grant, naming `router.writer_of(owner)`, when the writer cannot answer (it is
///      release tier, or not Present) → `Quit`. A push the runner routed to the writer never
///      falls to the read chain, whose head may have push rights of its own;
///    - otherwise (no nonce, no live grant for this slug, or a grant naming a profile other
///      than the writer) the owner routes as a Read (`decide`), and the first profile of the
///      read chain that is present answers. Anonymous → `Quit`.
///
///    A grant that names a release-tier profile never answers here (the model's
///    `ReleaseStaysHome`);
/// 6. release tier: only the live grant `asked.nonce` names (`GrantView::find` at
///    `asked.now`), for exactly this slug, whose spawner is this process's ancestor with the
///    grant's start, answers the grant's profile, and only when that profile is
///    `router.writer_of(owner)`, its row is release tier, and it is Present. No nonce, or
///    anything else → `Quit`.
///
/// Before rule 1, off unix, every request is `Quit`: no token file is read there, so the
/// helper answers nothing, and `agh whoami --git` says so.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AghGitGrant",
        action = "Answer",
        project = "agh::git::credential::project_answer"
    )
)]
#[must_use]
pub(crate) fn answer(
    req: &CredentialRequest,
    asked: &Asked<'_>,
    router: &Router,
    grants: &dyn GrantView,
    presence: &dyn Fn(&str) -> Presence,
) -> CredentialAnswer {
    explained(req, asked, router, grants, presence).0
}

/// `answer`, and whether a grant answered (rule 6, or rule 5's writer grant): what
/// `agh whoami --git` prints as `grant`.
pub(crate) fn explained(
    req: &CredentialRequest,
    asked: &Asked<'_>,
    router: &Router,
    grants: &dyn GrantView,
    presence: &dyn Fn(&str) -> Presence,
) -> (CredentialAnswer, bool) {
    if !cfg!(unix) {
        return (quit("agh's git helper answers only on unix"), false);
    }
    if asked.stdin_is_terminal {
        return (quit("stdin is a terminal: git asks, never a person"), false);
    }
    if req.op != "get" {
        let why = format!("{} is a no-op: agh keeps no credential for git", req.op);
        return (quit(why), false);
    }
    let Some(path) = req.path.as_deref() else {
        return (
            quit("no path: git must send credential.useHttpPath=true"),
            false,
        );
    };
    let Some(slug) = req.slug() else {
        let why = format!("{path} names no repository (want OWNER/REPO or OWNER/REPO.git)");
        return (quit(why), false);
    };
    if let Some(why) = foreign(req, asked) {
        return (quit(why), false);
    }
    let owner = slug.owner();
    let writer = router.writer_of(owner);
    let grant = asked
        .nonce
        .and_then(|nonce| grants.find(nonce, &slug, asked.now));
    if router.tier_of(owner) == Tier::Release {
        let answered = grant.filter(|grant| {
            writer == Some(grant.profile.as_str())
                && router
                    .profile(&grant.profile)
                    .is_some_and(|row| row.tier == Tier::Release)
                && presence(&grant.profile) == Presence::Present
        });
        return match answered {
            Some(grant) => (credential(router, &grant.profile), true),
            None => (
                quit(format!(
                    "{slug} is release tier: only a live grant for it answers"
                )),
                false,
            ),
        };
    }
    if let Some(grant) = grant.filter(|grant| writer == Some(grant.profile.as_str())) {
        let can = router
            .profile(&grant.profile)
            .is_some_and(|row| row.tier != Tier::Release)
            && presence(&grant.profile) == Presence::Present;
        if can {
            return (credential(router, &grant.profile), true);
        }
        let why = format!(
            "the grant's writer {} cannot answer for {slug}",
            grant.profile
        );
        return (quit(why), false);
    }
    let read = RouteRequest::read(RouteTarget::Repo(slug.clone()));
    match router.decide(&read, presence) {
        Decision::Send { profile, .. } => (credential(router, &profile), false),
        Decision::Anonymous { .. } => (
            quit(format!("the read chain for {owner} ends anonymous")),
            false,
        ),
        Decision::Refuse(refusal) => (quit(refusal.why), false),
    }
}

/// Rule 4: why the request's protocol and host are not the one place the helper answers, or
/// `None` when they are.
fn foreign(req: &CredentialRequest, asked: &Asked<'_>) -> Option<String> {
    let protocol = req.protocol.as_deref().unwrap_or_default();
    let host = req.host.as_deref().unwrap_or_default();
    if protocol == "https" && host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    #[cfg(any(test, debug_assertions))]
    if protocol == "http" {
        if asked.loopback == Some(host) {
            return None;
        }
        return Some(format!(
            "{protocol}://{host} is not this run's loopback endpoint"
        ));
    }
    #[cfg(not(any(test, debug_assertions)))]
    let _ = asked;
    Some(format!("{protocol}://{host} is not https://github.com"))
}

/// The credential of `profile`, with the tier the router holds for it.
fn credential(router: &Router, profile: &str) -> CredentialAnswer {
    match router.profile(profile) {
        Some(row) => CredentialAnswer::Credential {
            profile: profile.to_string(),
            tier: row.tier,
        },
        None => quit(format!("{profile} is not in agh.toml")),
    }
}

/// The model's view of an answer (`AghGitGrant`'s `(phase, used)`):
/// - a credential whose profile is release tier (`CredentialAnswer::Credential`'s `tier`) →
///   (1, 2);
/// - any other credential → (1, 1);
/// - `Quit` → (2, 0).
///
/// It reads the tier of the profile that answered, never the slug's. So a release credential
/// answered for a dev slug projects (1, 2), which breaks `ReleaseStaysHome`.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_answer(answer: &CredentialAnswer) -> (i64, i64) {
    match answer {
        CredentialAnswer::Credential {
            tier: Tier::Release,
            ..
        } => (1, 2),
        CredentialAnswer::Credential { .. } => (1, 1),
        CredentialAnswer::Quit { .. } => (2, 0),
    }
}

/// The helper's three streams, as `serve_with` takes them. One struct, so that `serve_with`
/// keeps to plan §1 rule 4's seven parameters.
pub struct HelperIo<'a> {
    /// git's request.
    pub stdin: &'a mut dyn std::io::Read,
    /// Whether that stdin is a terminal (rule 1).
    pub stdin_is_terminal: bool,
    /// Where the answer goes.
    pub stdout: &'a mut dyn std::io::Write,
}

/// `name` in `env`, when it is set and UTF-8.
pub(crate) fn var(env: &dyn EnvSource, name: &str) -> Option<String> {
    env.var_os(name).and_then(|value| value.into_string().ok())
}

/// could_not_run, `Sub::Local`: the answer could not be written to git.
fn unwritten(e: &std::io::Error) -> Error {
    Error::local(format!("the git credential answer was not written: {e}"))
}

/// The helper's refusal: `quit=1\n` to `stdout`, flushed, which tells git to stop asking.
///
/// # Errors
///
/// could_not_run, `Sub::Local`, why `the git credential answer was not written: {e}`.
pub(crate) fn write_quit(stdout: &mut dyn std::io::Write) -> Result<()> {
    stdout
        .write_all(b"quit=1\n")
        .and_then(|()| stdout.flush())
        .map_err(|e| unwritten(&e))
}

/// The whole helper: read the request from `io.stdin`, decide, load the profile's token and
/// print `username=…\npassword=…\n` to `io.stdout`, or `quit=1\n`. A token that does not load
/// is `quit=1`. Off unix it always answers `quit=1`. For a profile in `pinned` it sends exactly
/// that credential in place of a fresh load of the profile's file: the tokens vault adoption
/// judged (`Adoption::pinned`), so a vault file rewritten after adoption judged it never
/// speaks for the rows adoption derived from it (design §6.10); with no pins every profile's
/// token is loaded fresh. A stdin that is a terminal, and an op other than `get`, is
/// `quit=1` before anything is read from it.
///
/// # Errors
///
/// Only a write to `stdout` that failed; every refusal is `quit=1`. The nonce is `AGH_GIT_GRANT`
/// in `env`, the loopback host `AGH_GIT_LOOPBACK` (read only in a test or debug build), and
/// `now` the caller's `Fx::wall_secs`.
pub fn serve_with(
    op: &str,
    io: HelperIo<'_>,
    router: &Router,
    grants: &dyn GrantView,
    env: &dyn EnvSource,
    now: u64,
    pinned: &[std::sync::Arc<crate::identity::Credential>],
) -> Result<()> {
    let HelperIo {
        stdin,
        stdin_is_terminal,
        stdout,
    } = io;
    // Rule 2 before the read too: git writes `store` and `erase` with the credential it used,
    // `password=` included, and an op that answers nothing never holds that token in a buffer
    // nothing scrubs. git ignores SIGPIPE while it writes a helper's input.
    if !cfg!(unix) || stdin_is_terminal || op != "get" {
        return write_quit(stdout);
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(REQUEST_MAX)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if std::io::Read::read_to_end(&mut std::io::Read::take(stdin, limit), &mut bytes).is_err() {
        return write_quit(stdout);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return write_quit(stdout);
    };
    let Ok(req) = CredentialRequest::parse(op, text) else {
        return write_quit(stdout);
    };
    let nonce = var(env, crate::git::GRANT_VAR);
    #[cfg(any(test, debug_assertions))]
    let loopback = var(env, crate::git::LOOPBACK_VAR);
    #[cfg(not(any(test, debug_assertions)))]
    let loopback: Option<String> = None;
    let asked = Asked {
        stdin_is_terminal,
        nonce: nonce.as_deref(),
        loopback: loopback.as_deref(),
        now,
    };
    let presence = |name: &str| router.presence(name, env);
    let CredentialAnswer::Credential { profile, .. } =
        answer(&req, &asked, router, grants, &presence)
    else {
        return write_quit(stdout);
    };
    let cred = match pinned.iter().find(|cred| cred.profile() == profile) {
        Some(cred) => std::sync::Arc::clone(cred),
        None => {
            let Some(row) = router.profile(&profile) else {
                return write_quit(stdout);
            };
            let Ok(cred) = crate::identity::load(row, env) else {
                return write_quit(stdout);
            };
            std::sync::Arc::new(cred)
        }
    };
    let written = (|| {
        stdout.write_all(b"username=")?;
        stdout.write_all(super::GIT_USERNAME.as_bytes())?;
        stdout.write_all(b"\npassword=")?;
        stdout.write_all(cred.expose())?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    })();
    written.map_err(|e| unwritten(&e))
}

/// What the helper routes by: the router, the credentials vault adoption judged (which the
/// helper sends for their profiles in place of a fresh load, `serve_with`), and adoption's
/// step for a vault token it found and did not route.
pub(crate) struct HelperRoute {
    /// The router.
    pub(crate) router: Router,
    /// `Adoption::pinned`, or none.
    pub(crate) pinned: Vec<std::sync::Arc<crate::identity::Credential>>,
    /// `Adoption::next_step`, or none.
    #[cfg(unix)]
    pub(crate) next: Option<String>,
    /// Whether vault adoption built the router (no agh.toml, or one that says `vault = true`),
    /// so a `dev` or `release` row came from the vault adoption record.
    #[cfg(unix)]
    pub(crate) adopted: bool,
}

impl HelperRoute {
    /// The route over `router` with nothing adopted: no pinned credential, no adoption step.
    pub(crate) fn over(router: Router) -> HelperRoute {
        HelperRoute {
            router,
            pinned: Vec::new(),
            #[cfg(unix)]
            next: None,
            #[cfg(unix)]
            adopted: false,
        }
    }

    /// The route that answers nothing: [`Router::empty`], nothing adopted.
    pub(crate) fn empty() -> HelperRoute {
        HelperRoute::over(Router::empty())
    }

    /// Whether the helper answers a request for `slug` as `profile`, by the rules that pick
    /// its profile (rules 5 and 6) over its own router, with `serve_with`'s presence. A run
    /// that writes a grant naming `profile` (`granted`: a leased delete, or a release-tier
    /// read) is answered as `profile` only when it is the helper's `writer_of(owner)`, its row
    /// is release tier exactly when the owner is, and it is Present; a grant naming any other
    /// profile answers nothing of its own. A run with no grant is answered by the read chain:
    /// the owner routed as a Read must name `profile`.
    #[cfg(unix)]
    pub(crate) fn answers_as(
        &self,
        slug: &Slug,
        profile: &str,
        granted: bool,
        env: &dyn EnvSource,
    ) -> bool {
        let router = &self.router;
        let presence = |name: &str| router.presence(name, env);
        let owner = slug.owner();
        let release = router.tier_of(owner) == Tier::Release;
        if granted {
            return router.writer_of(owner) == Some(profile)
                && router
                    .profile(profile)
                    .is_some_and(|row| (row.tier == Tier::Release) == release)
                && presence(profile) == Presence::Present;
        }
        if release {
            return false;
        }
        let read = RouteRequest::read(RouteTarget::Repo(slug.clone()));
        matches!(router.decide(&read, &presence), Decision::Send { profile: named, .. } if named == profile)
    }
}

/// The helper's route, as the runner's client routes (`Ctx::github`), asking GitHub nothing:
/// `route_for(env, dirs, loose)`, `loose` when `env` holds `AGH_GIT_LOOSE=1` (a runner sets it
/// only for a profile whose `legacy_loose()` is true).
///
/// # Errors
///
/// As `route_for`.
pub(crate) fn helper_route(env: &dyn EnvSource, dirs: &crate::config::Dirs) -> Result<HelperRoute> {
    let loose = env
        .var_os(crate::git::LOOSE_VAR)
        .is_some_and(|value| value == "1");
    route_for(env, dirs, loose)
}

/// The route the helper takes under `loose` or not. Not loose: `config::load(dirs)`, then,
/// with no agh.toml or one that says `vault = true`, vault adoption as the record and the files
/// stand (`vault::view`, design §6.10), whose rows route and whose judged tokens are pinned.
/// Loose: the same shared view with AghHost's explicit `legacy_loose()` profile policy;
/// vault credentials remain strict. A token the record does not bind adopts nothing
/// here, which is why the runner checks the helper's route before it runs (`answers_as`): a
/// runner whose call could not record a learned login refuses the run instead of spawning a
/// git its helper will not answer.
///
/// # Errors
///
/// `config::load`'s (or `load_for_host`'s) refusal of an invalid agh.toml, then adoption's
/// refusal (`vault::Adoption::router`).
pub(crate) fn route_for(
    env: &dyn EnvSource,
    dirs: &crate::config::Dirs,
    loose: bool,
) -> Result<HelperRoute> {
    let resolved = if loose {
        crate::routing::view_for_host(dirs, env)?
    } else {
        crate::routing::view(dirs, env)?
    };
    if let Some(adoption) = resolved.adoption() {
        return Ok(HelperRoute {
            router: resolved.router()?.clone(),
            pinned: adoption.pinned().to_vec(),
            #[cfg(unix)]
            next: adoption.next_step(),
            #[cfg(unix)]
            adopted: true,
        });
    }
    Ok(HelperRoute::over(resolved.router()?.clone()))
}

#[cfg(test)]
mod tests {
    use super::{Asked, CredentialAnswer, CredentialRequest, explained};
    use crate::git::grant::MapGrants;
    use crate::policy::{Presence, Router};

    /// One dev profile from the environment, reading and writing for `fixture-dev`: no file is
    /// read, so the decision is the same on every OS but for the off-unix rule.
    fn router() -> Router {
        let text = "[profiles.dev]\nenv = \"AGH_TEST_TOKEN\"\ntier = \"dev\"\n\
                    login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                    tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n";
        let parsed =
            crate::config::parse(text).unwrap_or_else(|f| panic!("{}: {}", f.rule, f.what));
        let validated = crate::config::validate(&parsed, std::path::Path::new("/"), &|_| None)
            .unwrap_or_else(|faults| panic!("{} faults", faults.len()));
        Router::from_validated(validated)
    }

    /// `whoami --git` explains what the helper answers: off unix that is always a quit, as
    /// `serve_with` answers there, even for a profile that is Present.
    #[test]
    fn the_helper_answers_only_on_unix() {
        let req = CredentialRequest::parse(
            "get",
            "protocol=https\nhost=github.com\npath=fixture-dev/app.git\n\n",
        )
        .unwrap_or_else(|e| panic!("the request: {}", e.why));
        let (answer, granted) = explained(
            &req,
            &Asked::default(),
            &router(),
            &MapGrants::new(),
            &|_| Presence::Present,
        );
        assert!(!granted);
        assert_eq!(
            matches!(answer, CredentialAnswer::Credential { .. }),
            cfg!(unix),
            "{answer:?}"
        );
    }
}
