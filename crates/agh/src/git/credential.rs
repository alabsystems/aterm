// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth git-credential get|store|erase` (design §6.9): git's helper protocol.

use crate::error::{Error, Result};
use crate::identity::EnvSource;
use crate::policy::{Presence, Router, Tier};
use crate::slug::Slug;

use super::grant::GrantView;

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

impl CredentialRequest {
    /// The request read from git's text. Lines past 64 KiB, a line with no `=`, or a NUL are
    /// a malformed request.
    ///
    /// # Errors
    ///
    /// Usage, why `git sent a malformed credential request: {what}`.
    pub fn parse(op: &str, text: &str) -> Result<CredentialRequest> {
        let _ = (op, text);
        Err(Error::not_yet("the credential request's parse", "WP7"))
    }

    /// The slug the path names: `<owner>/<repo>` or `<owner>/<repo>.git`.
    #[must_use]
    pub fn slug(&self) -> Option<Slug> {
        // WP7 writes the body (plan §11); until then no path names a repository.
        None
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
///    grant's start, answers the grant's profile. No nonce, or anything else → `Quit`.
#[must_use]
pub(crate) fn answer(
    req: &CredentialRequest,
    asked: &Asked<'_>,
    router: &Router,
    grants: &dyn GrantView,
    presence: &dyn Fn(&str) -> Presence,
) -> CredentialAnswer {
    let _ = (req, asked, router, grants, presence);
    CredentialAnswer::Quit {
        why: "not yet: the helper's decision lands in WP7".to_string(),
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

/// The helper's three streams, as `serve` takes them. One struct, so that `serve` keeps to
/// plan §1 rule 4's seven parameters.
pub struct HelperIo<'a> {
    /// git's request.
    pub stdin: &'a mut dyn std::io::Read,
    /// Whether that stdin is a terminal (rule 1).
    pub stdin_is_terminal: bool,
    /// Where the answer goes.
    pub stdout: &'a mut dyn std::io::Write,
}

/// The whole helper: read the request from `io.stdin`, decide, load the profile's token and
/// print `username=…\npassword=…\n` to `io.stdout`, or `quit=1\n`. A token that does not load
/// is `quit=1`.
///
/// # Errors
///
/// Only a write to `stdout` that failed; every refusal is `quit=1`. The nonce is `AGH_GIT_GRANT`
/// in `env`, the loopback host `AGH_GIT_LOOPBACK` (read only in a test or debug build), and
/// `now` the caller's `Fx::wall_secs`.
pub fn serve(
    op: &str,
    io: HelperIo<'_>,
    router: &Router,
    grants: &dyn GrantView,
    env: &dyn EnvSource,
    now: u64,
) -> Result<()> {
    let _ = (op, io, router, grants, env, now);
    Err(Error::not_yet("the git credential helper", "WP7"))
}
