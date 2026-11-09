// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The isolated git runner (design §6.9): what runs, with which environment, as whom, and
//! what it printed. Unix only (plan §1, rule 12).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::client::Github;
use crate::error::{Error, Result};
use crate::policy::Named;
use crate::slug::Slug;

use super::GIT_BUDGET;

/// An anonymous smart-HTTP child has no whole-transfer latency budget. It may run for as long
/// as bytes or descendant CPU/I/O keep advancing, but one minute without that evidence ends the
/// attempt and lets the shared idempotent-read policy decide whether another attempt is allowed.
#[cfg(unix)]
const ANONYMOUS_READ_PROGRESS_WINDOW: Duration = Duration::from_secs(60);

/// What the runner runs. There is no free-form argv.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitOp {
    /// Bounded commit-only object intake and exact parent ancestry. General
    /// fetch semantics are unchanged; no ref/tree/checkout promise is made.
    CommitRead {
        /// Screened existing repository whose immutable objects are reused.
        dir: PathBuf,
        /// Validated exact objects and ancestry questions.
        request: super::CommitRead,
        /// An explicit credential-free read, never a fallback.
        anonymous: bool,
    },
    /// Admit local object reads under the same source-config and discovery guards.
    /// No remote request, credentials, grant or source update participates.
    CheckLocal {
        /// Existing source checkout or bare repository.
        dir: PathBuf,
    },
    /// `git clone --quiet [--depth N] -- <url> <private-stage>`, then atomically publish the
    /// stage at `<dest>`. `<dest>` must be absent or empty and is never a retry workspace.
    Clone {
        /// Where to.
        dest: PathBuf,
        /// `--depth`.
        depth: Option<u32>,
    },
    /// `git clone --quiet --mirror -- <url> <private-stage>`: a bare snapshot of all refs,
    /// atomically published at `<dest>` after success.
    Mirror {
        /// An absent or empty destination.
        dest: PathBuf,
    },
    /// `git -C <dir> fetch --quiet --tags --force --recurse-submodules=no <url>
    /// +refs/heads/*:refs/remotes/origin/*`: atpkg-publish's `fetch_repo`, by URL. git reads
    /// a `remote.<URL>.*` section as that URL's remote, so the runner's config screen refuses
    /// a clone section named by a URL. `--recurse-submodules=no` overrides the clone's own
    /// `submodule.recurse` and `fetch.recurseSubmodules`, so no submodule is fetched over a
    /// URL or a transport the runner did not choose while `AGH_GIT_GRANT` is set.
    Fetch {
        /// The clone.
        dir: PathBuf,
    },
    /// `git -C <dir> -c core.logAllRefUpdates=always fetch --quiet --no-tags
    /// --no-write-fetch-head --recurse-submodules=no
    /// <url> +refs/heads/main:<into>`: atpkg-publish's `fetch_main`, which reads the main of
    /// record into a person's source checkout and writes no other ref there, so neither its
    /// remote-tracking refs nor its tags move. A per-command `core.logAllRefUpdates=always`
    /// retains previous snapshots in a reflog, without changing repository configuration.
    /// Its config screen is `Fetch`'s. The runner
    /// refuses it (Usage, before anything runs) unless `into` is a ref name git accepts under
    /// `refs/` that is not under `refs/heads/`, `refs/tags/` or `refs/remotes/`, so the forced
    /// fetch never moves a branch, a tag or a remote-tracking ref of the checkout.
    FetchMain {
        /// The checkout.
        dir: PathBuf,
        /// The ref main is fetched into.
        into: String,
    },
    /// Fetch one full ref or object id into agh's private local namespace, without
    /// changing FETCH_HEAD, branches, tags or remote-tracking refs.
    FetchRef {
        /// The existing checkout or bare repository.
        dir: PathBuf,
        /// A full ref or forty-digit object id.
        reference: String,
        /// A ref beneath `refs/agh/`.
        into: String,
        /// Read without any credential, including for a release-tier owner.
        anonymous: bool,
    },
    /// Fetch an admitted exact snapshot with one network transfer, then atomically
    /// update only its private cache refs after every expected OID matches.
    FetchBatch {
        /// The existing checkout or bare repository.
        dir: PathBuf,
        /// Validated source, destination and expected raw OID rows.
        batch: super::FetchBatch,
        /// Read without a credential or fallback identity.
        anonymous: bool,
    },
    /// `git ls-remote --refs <url> [<ref>]`; exact HEAD includes that pseudoref.
    LsRemote {
        /// One ref, or every one.
        reference: Option<String>,
        /// Use credential-free smart HTTP and include peeled tag targets.
        anonymous: bool,
    },
    /// `git push --porcelain --force-with-lease=<ref>:<expect> <url> :<ref>`, from an empty
    /// scratch repository (`git init --quiet --bare` in the run's private scratch), so no
    /// local ref or hook is near it. It speaks as the owner's writer. The runner refuses it
    /// (Usage, before anything runs) when `reference` fails the reference rule or `expect` is
    /// not 40 lowercase hex, because `GitRun` is public and both go into the argv:
    /// `Refs::delete_expect` checks them first, and the runner checks again.
    PushDelete {
        /// The ref.
        reference: String,
        /// The sha it must point at.
        expect: String,
    },
    /// Atomically push exact local objects with per-ref compare-and-swap leases.
    PushRefs {
        /// Caller-owned source repository, screened but never modified.
        dir: PathBuf,
        /// Validated explicit remote refs and old/new object IDs.
        batch: crate::github::refs::AtomicRefBatch,
        /// Run the same checks and Git's dry run without changing remote refs.
        dry_run: bool,
    },
    /// The exact release lease/fence protocol. Actual mutation is admitted only
    /// through its journaled resource boundary, never a direct `run_git` call.
    PushControl {
        /// Caller-owned source repository, screened and read without changes.
        dir: PathBuf,
        /// Validated canonical control names and expected old/new identities.
        transaction: crate::git::ControlRefTransaction,
        /// Validate the same transaction without changing remote refs.
        dry_run: bool,
    },
}

/// One isolated git run on one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitRun {
    /// The repository.
    pub slug: Slug,
    /// What runs.
    pub op: GitOp,
    /// How the destination was named: a release-tier owner needs `Named::Literal`.
    pub named: Named,
    /// The local, authenticated or mutating run's wall bound (`GIT_BUDGET` by default), and the
    /// upper lifetime supplied to a credential grant. [`Self::progress_window`] replaces it as
    /// the execution bound; anonymous network children always use a progress bound.
    pub budget: Duration,
    /// An inactivity window for native progress. When set, child execution is bounded by
    /// output and descendant CPU/I/O progress instead of [`Self::budget`]'s wall clock.
    pub progress_window: Option<Duration>,
}

impl GitRun {
    /// Exact commit-only reads, with no whole-history fetch or worker ref writes.
    #[must_use]
    pub fn commit_read(
        slug: &Slug,
        dir: &Path,
        request: super::CommitRead,
        anonymous: bool,
    ) -> GitRun {
        GitRun::of(
            slug,
            GitOp::CommitRead {
                dir: dir.to_path_buf(),
                request,
                anonymous,
            },
        )
    }
    /// Screen one local source for reads, without consulting a GitHub identity.
    #[must_use]
    pub fn check_local(slug: &Slug, dir: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::CheckLocal {
                dir: dir.to_path_buf(),
            },
        )
        .with_budget(Duration::from_secs(30))
    }
    /// Push only these local object IDs, with exact leases and atomicity.
    #[must_use]
    pub fn push_refs(
        slug: &Slug,
        dir: &Path,
        batch: crate::github::refs::AtomicRefBatch,
        dry_run: bool,
    ) -> GitRun {
        GitRun::of(
            slug,
            GitOp::PushRefs {
                dir: dir.to_path_buf(),
                batch,
                dry_run,
            },
        )
    }

    pub(crate) fn push_control(
        slug: &Slug,
        dir: &Path,
        transaction: crate::git::ControlRefTransaction,
        dry_run: bool,
    ) -> GitRun {
        GitRun::of(
            slug,
            GitOp::PushControl {
                dir: dir.into(),
                transaction,
                dry_run,
            },
        )
    }
    /// `op` on `slug`, `Named::Default`, with `GIT_BUDGET` for children that use a wall bound.
    fn of(slug: &Slug, op: GitOp) -> GitRun {
        GitRun {
            slug: slug.clone(),
            op,
            named: Named::Default,
            budget: GIT_BUDGET,
            progress_window: None,
        }
    }

    /// A clone of `slug` into `dest`. It is not named `clone`, because `GitRun` derives
    /// `Clone` and `run.clone()` keeps meaning a copy.
    #[must_use]
    pub fn new_clone(slug: &Slug, dest: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::Clone {
                dest: dest.to_path_buf(),
                depth: None,
            },
        )
    }

    /// A complete bare mirror, including custom ref namespaces, into `dest`.
    #[must_use]
    pub fn new_mirror(slug: &Slug, dest: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::Mirror {
                dest: dest.to_path_buf(),
            },
        )
    }

    /// A fetch of `slug` into the clone `dir`.
    #[must_use]
    pub fn fetch(slug: &Slug, dir: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::Fetch {
                dir: dir.to_path_buf(),
            },
        )
    }

    /// A fetch of `slug`'s `refs/heads/main` into the ref `into` of the checkout `dir`.
    #[must_use]
    pub fn fetch_main(slug: &Slug, dir: &Path, into: &str) -> GitRun {
        GitRun::of(
            slug,
            GitOp::FetchMain {
                dir: dir.to_path_buf(),
                into: into.to_string(),
            },
        )
    }

    /// Fetch exactly `reference` into `refs/agh/…`. Anonymous reads never get a
    /// credential helper or a release-writer grant.
    #[must_use]
    pub fn fetch_ref(
        slug: &Slug,
        dir: &Path,
        reference: &str,
        into: &str,
        anonymous: bool,
    ) -> GitRun {
        GitRun::of(
            slug,
            GitOp::FetchRef {
                dir: dir.to_path_buf(),
                reference: reference.to_string(),
                into: into.to_string(),
                anonymous,
            },
        )
    }

    /// Fetch an entire admitted snapshot without moving caller branches, tags,
    /// tracking refs, configuration or FETCH_HEAD.
    #[must_use]
    pub fn fetch_batch(
        slug: &Slug,
        dir: &Path,
        batch: super::FetchBatch,
        anonymous: bool,
    ) -> GitRun {
        GitRun::of(
            slug,
            GitOp::FetchBatch {
                dir: dir.to_path_buf(),
                batch,
                anonymous,
            },
        )
    }

    /// `ls-remote` of `slug`.
    #[must_use]
    pub fn ls_remote(slug: &Slug, reference: Option<&str>) -> GitRun {
        GitRun::of(
            slug,
            GitOp::LsRemote {
                reference: reference.map(str::to_string),
                anonymous: false,
            },
        )
    }

    /// Credential-free smart HTTP advertisement, including peeled annotated tags.
    #[must_use]
    pub fn public_refs(slug: &Slug) -> GitRun {
        GitRun::of(
            slug,
            GitOp::LsRemote {
                reference: None,
                anonymous: true,
            },
        )
    }

    /// The leased delete of `reference` at `expect`.
    #[must_use]
    pub fn push_delete(slug: &Slug, reference: &str, expect: &str) -> GitRun {
        GitRun::of(
            slug,
            GitOp::PushDelete {
                reference: reference.to_string(),
                expect: expect.to_string(),
            },
        )
    }

    /// The same run, named (a release-tier owner's run needs a literal or a grant).
    #[must_use]
    pub fn with_named(mut self, named: Named) -> GitRun {
        self.named = named;
        self
    }

    /// The same run with another local, authenticated or mutating wall bound and grant lifetime.
    /// It is not a whole-transfer latency bound for an anonymous network child.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> GitRun {
        self.budget = budget;
        self
    }

    /// The same run with a native-progress inactivity bound instead of a wall-clock execution
    /// bound. The ordinary budget remains the lifetime of any short-lived credential grant.
    #[must_use]
    pub fn with_progress_window(mut self, window: Duration) -> GitRun {
        self.progress_window = Some(window);
        self
    }

    /// The same ordinary clone, shallow. Any other op, including a mirror, is unchanged.
    #[must_use]
    pub fn with_depth(mut self, depth: u32) -> GitRun {
        if let GitOp::Clone { depth: shallow, .. } = &mut self.op {
            *shallow = Some(depth);
        }
        self
    }
}

/// What a run printed, redacted, and who it spoke as. Ordinary streams keep
/// their last 64 KiB; commit-only observations are complete within 256 KiB.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitOut {
    /// stdout.
    pub stdout: String,
    /// stderr.
    pub stderr: String,
    /// The profile the helper would answer: `None` for a dev-tier read that went anonymous,
    /// and the owner's writer for either push operation.
    pub as_: Option<String>,
    /// The grant's nonce, for a release-tier read or either push operation.
    pub grant: Option<String>,
}

/// The command git runs as its credential helper: this executable, and the words before
/// `auth git-credential`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperCommand {
    /// The executable, absolute and canonical.
    pub exe: PathBuf,
    /// The words before `auth git-credential`: `["agh"]` for `aterm`, `aterm-release`
    /// and `atpkg-publish`, none for an executable whose file stem is `agh`.
    pub args: Vec<String>,
}

/// could_not_run, `Sub::Local`, why `the helper's executable cannot be named: {what}`.
fn unnamed(what: &str) -> Error {
    Error::local(format!("the helper's executable cannot be named: {what}"))
}

impl HelperCommand {
    /// `std::env::current_exe()`, canonicalized, with `args` by its file stem: `["agh"]` for
    /// `aterm`, `aterm-release` and `atpkg-publish`, none for `agh`.
    ///
    /// # Errors
    ///
    /// could_not_run, why `the helper's executable cannot be named: {e}`, or, for any other
    /// file stem, `… {stem} is not agh, aterm, aterm-release or atpkg-publish`.
    pub fn current() -> Result<HelperCommand> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| unnamed(&e.to_string()))?;
        Self::for_executable(exe)
    }

    fn for_executable(exe: PathBuf) -> Result<HelperCommand> {
        let stem = exe
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let args = match stem.as_str() {
            "agh" => Vec::new(),
            "aterm" | "aterm-release" | "atpkg-publish" => vec!["agh".to_string()],
            _ => {
                return Err(unnamed(&format!(
                    "{stem} is not agh, aterm, aterm-release or atpkg-publish"
                )));
            }
        };
        Ok(HelperCommand { exe, args })
    }

    /// The `credential.helper` value: `!` and the shell-quoted words, then
    /// `auth git-credential`. Each word is wrapped in `"…"`, with `"`, `\`, `$` and a backtick
    /// escaped by `\`.
    #[must_use]
    pub fn config_value(&self) -> String {
        let quote = |word: &str| {
            let mut quoted = String::with_capacity(word.len() + 2);
            quoted.push('"');
            for c in word.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    quoted.push('\\');
                }
                quoted.push(c);
            }
            quoted.push('"');
            quoted
        };
        let exe = self.exe.to_string_lossy();
        let words: Vec<String> = std::iter::once(quote(&exe))
            .chain(self.args.iter().map(|word| quote(word)))
            .collect();
        format!("!{} auth git-credential", words.join(" "))
    }
}

/// The runner (`Github::run_git`): route, refuse a rewriting config, isolate the child's
/// environment and directories, write a grant when one is needed, spawn `git` through
/// `Fx::git`, clean up and map the exit (design §6.9).
///
/// # Errors
///
/// The refusals of each step; off unix `write::unix_only()`.
pub(crate) fn run(gh: &Github, run: &GitRun) -> Result<GitOut> {
    #[cfg(unix)]
    {
        unix::run(gh, run)
    }
    #[cfg(not(unix))]
    {
        let _ = (gh, run);
        Err(crate::write::unix_only())
    }
}

/// Whether `into` may take a forced fetch of main: a ref name git accepts under `refs/`
/// (`path::ref_name_ok`) that is no branch, tag or remote-tracking ref.
#[cfg(unix)]
fn into_ok(into: &str) -> bool {
    crate::path::ref_name_ok(into)
        && !["refs/heads/", "refs/tags/", "refs/remotes/"]
            .iter()
            .any(|kept| into.starts_with(kept))
}

#[cfg(unix)]
pub(crate) fn project_config_names(listed: &[u8], read_only: bool) -> Vec<u8> {
    unix::project_config_names(listed, read_only)
}

#[cfg(unix)]
mod unix {
    //! The runner's steps, on unix.

    use std::ffi::OsString;
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use super::{ANONYMOUS_READ_PROGRESS_WINDOW, GitOp, GitOut, GitRun};
    use crate::client::Github;
    use crate::config::Dirs;
    use crate::error::{Error, Forbid, Kind, Result, SideEffect, Sub};
    use crate::fx::child::{ChildOut, ChildSpec, ConfigQuery};
    use crate::git::GRANT_MAX;
    use crate::git::credential;
    use crate::git::grant::{self, Grant};
    use crate::identity::{EnvSource, Source};
    use crate::policy::{Decision, Named, RefusalCode, RouteRequest, RouteTarget, Tier};
    use crate::redact::Redactor;
    use crate::retry::RetryDecision;
    use crate::slug::Slug;
    use aterm_types::http_retry::{Cause, GitObservation};

    mod ancestry;

    /// The route door's fact naming where a profile's token lives (`Fx::fact`).
    const SOURCE: &str = "source";

    /// The canonical request of a leased delete's scratch repository, `git init --quiet
    /// --bare`: a fixed one, so a taped leased delete replays with nothing run.
    const INIT_SCRATCH: &str = "git init-scratch";
    /// The canonical request of a fetch's config read, followed by ` {owner}/{repo}`.
    const CONFIG_LIST: &str = "git config-screen-v2";
    /// How much of each stream a `GitOut` keeps: its last 64 KiB.
    const OUT_KEEP: usize = 64 * 1024;
    /// The longest config listing step 3 screens: 256 KiB of key names, far past any real
    /// clone's and well under the 1 MiB tail the child door keeps of a stream, so a listing
    /// the door cut is always longer than this and refused.
    const SCREEN_MAX: usize = 256 * 1024;

    #[derive(serde::Serialize, serde::Deserialize)]
    enum ConfigClass {
        Url,
        Http,
        Credential,
        Include,
        IncludeIf,
        Protocol,
        Bundle,
        Hook,
        Fetch,
        Remote,
        Core,
        Extensions,
        Unknown,
    }

    impl ConfigClass {
        fn of(key: &str) -> Self {
            let section = key.split_once('.').map_or(key, |(section, _)| section);
            match section.to_ascii_lowercase().as_str() {
                "url" => Self::Url,
                "http" => Self::Http,
                "credential" => Self::Credential,
                "include" => Self::Include,
                "includeif" => Self::IncludeIf,
                "protocol" => Self::Protocol,
                "bundle" => Self::Bundle,
                "hook" => Self::Hook,
                "fetch" => Self::Fetch,
                "remote" => Self::Remote,
                "core" => Self::Core,
                "extensions" => Self::Extensions,
                _ => Self::Unknown,
            }
        }

        fn key(&self) -> &'static str {
            match self {
                Self::Url => "url.*",
                Self::Http => "http.*",
                Self::Credential => "credential.*",
                Self::Include => "include.*",
                Self::IncludeIf => "includeif.*",
                Self::Protocol => "protocol.*",
                Self::Bundle => "bundle.*",
                Self::Hook => "hook.*",
                Self::Fetch => "fetch.*",
                Self::Remote => "remote.*",
                Self::Core => "core.*",
                Self::Extensions => "extensions.*",
                Self::Unknown => "repository-config.*",
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    enum ConfigVerdict {
        Allowed { keys: usize },
        TooLong { bytes: usize },
        Refused { class: ConfigClass },
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    enum ConfigSourceVerdict {
        Ready,
        Missing,
        Refused(String),
    }

    fn config_names_request(slug: &Slug, read_only: bool, scope: &str) -> Vec<u8> {
        let access = if read_only { "read-only" } else { "write" };
        format!("{CONFIG_LIST} {slug} names {access} {scope}").into_bytes()
    }
    /// The variables a git child gets from the client's environment.
    const PASSED: [&str; 6] = [
        "USER",
        "LOGNAME",
        "PATH",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ];

    /// Usage, before anything runs: a leased delete's reference or sha that would not go into
    /// the argv as one unambiguous word, or a clone into a directory that is not empty.
    fn check_op(run: &GitRun) -> Result<()> {
        match &run.op {
            GitOp::PushDelete { reference, expect } => {
                if !crate::path::reference_ok(reference) {
                    return Err(Error::usage(crate::path::reference_refusal(reference)));
                }
                if !crate::path::is_sha(expect) {
                    return Err(Error::usage(crate::path::sha_refusal(expect)));
                }
                Ok(())
            }
            GitOp::Clone { dest, .. } | GitOp::Mirror { dest } => {
                let empty = match std::fs::read_dir(dest) {
                    Ok(mut entries) => entries.next().is_none(),
                    Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                };
                if empty {
                    Ok(())
                } else {
                    Err(Error::usage(format!(
                        "{} is not an empty directory",
                        dest.display()
                    )))
                }
            }
            GitOp::FetchMain { into, .. } => {
                if super::into_ok(into) {
                    return Ok(());
                }
                Err(Error::usage(format!(
                    "{}: main is fetched into a ref under refs/ that is no branch, tag or \
                     remote-tracking ref, by a name git accepts",
                    Redactor::shapes_cow(into)
                )))
            }
            GitOp::FetchRef {
                reference, into, ..
            } => {
                if !(crate::path::is_sha(reference)
                    || (reference.starts_with("refs/") && crate::path::ref_name_ok(reference)))
                {
                    return Err(Error::usage(
                        "REF must be a full ref or forty lowercase hex digits",
                    ));
                }
                if !into.starts_with("refs/agh/") || !crate::path::ref_name_ok(into) {
                    return Err(Error::usage("--into must be a valid ref beneath refs/agh/"));
                }
                Ok(())
            }
            GitOp::PushRefs { batch, .. } => {
                if batch.updates().iter().any(|u| u.before == u.after) {
                    return Err(Error::usage(
                        "native push omits unchanged refs; remove before == after rows",
                    ));
                }
                Ok(())
            }
            GitOp::PushControl { .. } => Ok(()),
            GitOp::CommitRead { .. } => {
                if run.progress_window.is_some() {
                    return Err(Error::usage(
                        "commit ancestry owns its finite-work and native no-progress bounds",
                    ));
                }
                Ok(())
            }
            GitOp::CheckLocal { .. }
            | GitOp::Fetch { .. }
            | GitOp::FetchBatch { .. }
            | GitOp::LsRemote { .. } => Ok(()),
        }
    }

    /// Whether `named` is a literal of exactly `slug`: typed on agh's command line, or made by
    /// `Router::grant` from a `[[writers]]` row.
    fn literal_for(named: &Named, slug: &Slug) -> bool {
        matches!(named, Named::Literal(typed) if typed.slug() == slug)
    }

    /// Refused `release_needs_literal`: a release-tier run named by default or by `@target`.
    fn release_needs_literal(slug: &Slug) -> Error {
        let owner = slug.owner();
        Error::refused(format!(
            "a git run on release-tier {owner} needs the repository typed as owner/name, or a grant"
        ))
        .with_sub(Sub::Policy(RefusalCode::ReleaseNeedsLiteral))
        .with_next(format!("-R {slug}"))
        .with_repo(slug.to_string())
    }

    /// Refused: the profile the run would speak as keeps its token somewhere git's helper,
    /// another process that reads only token files, cannot read it. Where the token lives is a
    /// routing fact (`Fx::fact`), so a replay, whose router holds no row, refuses as the
    /// recording did.
    fn not_a_token_file(gh: &Github, profile: &str, slug: &Slug) -> Result<Option<Error>> {
        let router = gh.router();
        let source = gh.fx().fact(SOURCE, profile, &mut || match router
            .profile(profile)
            .map(|row| &row.source)
        {
            None | Some(Source::File(_)) => String::new(),
            Some(Source::Env(var)) => format!("env:{}", crate::config::masked(var)),
            Some(Source::Inline(_)) => "inline".to_string(),
        })?;
        if source.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            Error::refused(format!(
                "{profile}'s token is not in a token file ({source}), and git's credential helper reads only token files"
            ))
            .with_next(format!("route {slug} to a token_file profile in agh.toml"))
            .with_repo(slug.to_string()),
        ))
    }

    /// The grant's guard: its `Drop` removes the grant file, so a run that returns early or
    /// panics removes it too.
    struct GrantGuard<'a> {
        dirs: &'a Dirs,
        grant: Grant,
    }

    impl Drop for GrantGuard<'_> {
        fn drop(&mut self) {
            grant::remove(self.dirs, &self.grant);
        }
    }

    /// Where the run's git goes.
    enum Remote {
        /// `https://github.com/{owner}/{repo}.git`.
        Github(String),
        /// Tests and debug builds: the endpoints' loopback web origin, and its `host:port`.
        #[cfg(any(test, debug_assertions))]
        Loopback { url: String, host: String },
        /// Tests and debug builds: a local repository `git_remote_for_tests` named, as a
        /// `file://` URL.
        #[cfg(any(test, debug_assertions))]
        Local(String),
    }

    impl Remote {
        /// The remote of `slug` for `gh`.
        fn of(gh: &Github, slug: &Slug) -> Result<Remote> {
            #[cfg(any(test, debug_assertions))]
            {
                if let Some(path) = gh.git_remote(slug) {
                    let path = std::path::absolute(path)
                        .map_err(|e| Error::local(format!("{}: {e}", path.display())))?;
                    return Ok(Remote::Local(format!("file://{}", path.display())));
                }
                let web = gh.endpoints().origin(crate::wire::OriginRole::Web);
                if web.scheme() == "http" && web.host() == "127.0.0.1" {
                    let host = format!("127.0.0.1:{}", web.port());
                    let url = format!("http://{host}/{}/{}.git", slug.owner(), slug.repo());
                    return Ok(Remote::Loopback { url, host });
                }
            }
            #[cfg(not(any(test, debug_assertions)))]
            let _ = gh;
            Ok(Remote::Github(format!(
                "https://github.com/{}/{}.git",
                slug.owner(),
                slug.repo()
            )))
        }

        /// The URL git is handed.
        fn url(&self) -> &str {
            match self {
                Remote::Github(url) => url,
                #[cfg(any(test, debug_assertions))]
                Remote::Loopback { url, .. } | Remote::Local(url) => url,
            }
        }
    }

    /// What the run's children share: the program, the environment and the directories.
    #[derive(Clone)]
    struct Children<'a> {
        gh: &'a Github,
        /// The absolute `git`, or the bare `git` of a run nothing runs.
        git: PathBuf,
        /// The whole environment of every child.
        env: Vec<(OsString, OsString)>,
        /// `{scratch}/work`, or none for a run nothing runs.
        work: Option<PathBuf>,
        /// `{scratch}/scratch.git`, the leased delete's empty repository.
        scratch_git: Option<PathBuf>,
        /// The run's wall bound, used only when `progress_window` is absent.
        budget: std::time::Duration,
        /// A native-progress inactivity window in place of the wall-clock execution bound.
        progress_window: Option<std::time::Duration>,
        /// Whether the run only reads, so its main child killed at its bound changed nothing.
        /// The children the runner makes for itself (a fetch's config read, a leased delete's
        /// scratch `init`) send nothing whatever this says: `unsent` lowers their errors.
        reads: bool,
        /// This read carries no credential and may use the shared anonymous-HTTP retry policy.
        anonymous_read: bool,
        /// The private sibling directory a clone or mirror writes before one atomic publish.
        /// It is absent under replay, where no child or destination is changed.
        clone_stage: Option<PathBuf>,
    }

    /// The transport authority attached to one child. A private HTTP trace and the anonymous
    /// progress bound travel together, so an ordinary or authenticated child cannot acquire
    /// either independently.
    enum ChildNetwork<'a> {
        Ordinary,
        AnonymousHttp { trace: Option<&'a Path> },
    }

    /// A credential-free name for the canonical native operation, narrowed to its internal
    /// step when one is present. Timing policy is intentionally absent from canonical tape
    /// bytes, but those bytes are the safe vocabulary for a diagnostic too.
    pub(super) fn waiting_on(canonical: &[u8]) -> String {
        let Ok(text) = std::str::from_utf8(canonical) else {
            return "native git operation".to_owned();
        };
        let mut lines = text.lines();
        let first = lines
            .next()
            .filter(|line| line.starts_with("git "))
            .unwrap_or("native git operation");
        let step = lines.find_map(|line| line.strip_prefix("step "));
        step.map_or_else(|| first.to_owned(), |step| format!("{first}, step {step}"))
    }

    /// A child's error that sent nothing (the runner's own config read or scratch `init`,
    /// before any `git push` starts), lowered to `SideEffect::None`: killed at its bound or not
    /// waited for, it still never reached the remote.
    fn unsent(error: Error) -> Error {
        error.with_side_effect(SideEffect::None)
    }

    impl Children<'_> {
        /// One child through `Fx::git`: `args`, in `cwd` (or the work directory), its request
        /// named `canonical`. An error keeps `Fx::git`'s side effect unless the run only reads.
        fn git(&self, args: &[OsString], cwd: Option<&Path>, canonical: &[u8]) -> Result<ChildOut> {
            self.git_supplied(args, cwd, canonical, None, None, ChildNetwork::Ordinary)
        }

        fn git_input(
            &self,
            args: &[OsString],
            cwd: Option<&Path>,
            canonical: &[u8],
            input: &[u8],
        ) -> Result<ChildOut> {
            self.git_supplied(
                args,
                cwd,
                canonical,
                Some(input),
                None,
                ChildNetwork::Ordinary,
            )
        }

        fn config(&self, query: ConfigQuery, input: &[u8], canonical: &[u8]) -> Result<ChildOut> {
            let args: Vec<_> = query.args().iter().map(|arg| os(*arg)).collect();
            self.git_supplied(
                &args,
                None,
                canonical,
                Some(input),
                Some(query),
                ChildNetwork::Ordinary,
            )
        }

        fn git_supplied(
            &self,
            args: &[OsString],
            cwd: Option<&Path>,
            canonical: &[u8],
            input: Option<&[u8]>,
            config_query: Option<ConfigQuery>,
            network: ChildNetwork<'_>,
        ) -> Result<ChildOut> {
            let mut spec = ChildSpec::new(&self.git).budget(self.budget);
            let (curl_trace, progress_window) = match network {
                ChildNetwork::Ordinary => (None, self.progress_window),
                ChildNetwork::AnonymousHttp { trace } => (
                    trace,
                    self.progress_window
                        .or(Some(ANONYMOUS_READ_PROGRESS_WINDOW)),
                ),
            };
            if let Some(window) = progress_window {
                spec = spec.progressing(window, waiting_on(canonical));
            }
            if let Some(input) = input {
                spec = match config_query {
                    Some(query) => spec.config_input(input, query),
                    None => spec.input(input),
                };
            }
            for arg in args {
                spec = spec.arg(arg.clone());
            }
            for (name, value) in &self.env {
                spec.env.push((name.clone(), value.clone()));
            }
            if let Some(trace) = curl_trace {
                spec.env.push((
                    OsString::from("GIT_TRACE_CURL"),
                    trace.as_os_str().to_os_string(),
                ));
                spec.env.push((
                    OsString::from("GIT_TRACE_CURL_NO_DATA"),
                    OsString::from("1"),
                ));
                spec.env
                    .push((OsString::from("GIT_TRACE_REDACT"), OsString::from("1")));
            }
            if let Some(dir) = cwd.or(self.work.as_deref()) {
                spec = spec.cwd(dir);
            }
            let out = self.gh.fx().git(&spec, canonical);
            if self.reads {
                out.map_err(|e| e.with_side_effect(SideEffect::None))
            } else {
                out
            }
        }

        /// One remote read, under the shared anonymous-HTTP policy only when this run carries
        /// no credential. The private curl trace contains headers but no body, and Git's
        /// redaction is explicitly on; it exists only to recover a Retry-After that Git
        /// otherwise withholds from stderr.
        fn remote_read(
            &self,
            run: &GitRun,
            args: &[OsString],
            cwd: Option<&Path>,
            canonical: &[u8],
            input: Option<&[u8]>,
        ) -> Result<ChildOut> {
            if !self.anonymous_read {
                return self.git_supplied(
                    args,
                    cwd,
                    canonical,
                    input,
                    None,
                    ChildNetwork::Ordinary,
                );
            }
            let mut budget = self.gh.retry_policy().for_anonymous_http().budget();
            let mut attempt = 1u32;
            loop {
                let trace = self
                    .work
                    .as_deref()
                    .and_then(Path::parent)
                    .map(|root| root.join(format!("anonymous-http-{attempt}.trace")));
                if let Some(trace) = &trace {
                    let _ = std::fs::remove_file(trace);
                }
                let result = self.git_supplied(
                    args,
                    cwd,
                    canonical,
                    input,
                    None,
                    ChildNetwork::AnonymousHttp {
                        trace: trace.as_deref(),
                    },
                );
                if matches!(&result, Ok(out) if out.code == Some(0)) {
                    if let Some(trace) = &trace {
                        let _ = std::fs::remove_file(trace);
                    }
                    return result;
                }
                let now = self.gh.fx().wall_secs()?;
                let observed = trace
                    .as_deref()
                    .map(|path| git_trace_observation(path, Some(now)))
                    .unwrap_or_default();
                let cause = match &result {
                    Ok(out) => git_output_cause(out, observed),
                    Err(error) => git_error_cause(error),
                };
                let Some(cause) = cause else {
                    return result;
                };
                let retry = match budget.after(attempt, cause) {
                    RetryDecision::Retry(retry) => retry,
                    RetryDecision::Exhausted { bound } => {
                        let error = match result {
                            Ok(out) => failed(&run.op, &out, self.gh.redactor(), &run.slug, false),
                            Err(error) => error.with_repo(run.slug.to_string()),
                        };
                        return Err(retry_exhausted(error, &bound));
                    }
                    RetryDecision::Final => return result,
                };
                clear_clone_stage(self.clone_stage.as_deref())?;
                self.gh.fx().note_retry(retry)?;
                self.gh.fx().sleep(retry.pause)?;
                attempt = attempt.saturating_add(1);
            }
        }
    }

    fn retry_exhausted(mut error: Error, bound: &str) -> Error {
        error.why = format!("{} after {bound}", error.why);
        error
    }

    fn git_error_cause(error: &Error) -> Option<Cause> {
        (error.kind == Kind::CouldNotRun
            && error.sub == Sub::Local
            && (error.why.contains(" did not finish in ")
                || (error.why.contains("no stdout/stderr bytes")
                    && error.why.contains("made no CPU or I/O progress"))))
        .then_some(Cause::Timeout)
    }

    fn git_output_cause(out: &ChildOut, observed: GitObservation) -> Option<Cause> {
        let stderr = String::from_utf8_lossy(&out.stderr);
        observed.cause(&stderr)
    }

    /// Read only the tail of Git's private headers-only curl trace, then remove it. A trace
    /// failure removes Retry-After evidence; it never broadens the retry class.
    fn git_trace_observation(path: &Path, now_unix_secs: Option<u64>) -> GitObservation {
        const KEEP: u64 = 256 * 1024;
        let mut bytes = Vec::new();
        let read = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::open(path)?;
            let len = file.metadata()?.len();
            if len > KEEP {
                file.seek(SeekFrom::Start(len - KEEP))?;
            }
            file.read_to_end(&mut bytes)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(path);
        if read.is_err() {
            return GitObservation::default();
        }
        let text = String::from_utf8_lossy(&bytes);
        GitObservation::from_trace_at(&text, now_unix_secs)
    }

    /// Remove only entries in this run's private clone stage, so the same idempotent read can
    /// start again. A caller destination is never an attempt's workspace and is never cleaned.
    fn clear_clone_stage(stage: Option<&Path>) -> Result<()> {
        let Some(stage) = stage else {
            return Ok(());
        };
        let entries = match std::fs::read_dir(stage) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(Error::local(format!(
                    "cannot reset the private clone stage: {error}"
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                Error::local(format!("cannot reset the private clone stage: {error}"))
            })?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| {
                Error::local(format!("cannot reset the private clone stage: {error}"))
            })?;
            let removed = if kind.is_dir() && !kind.is_symlink() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.map_err(|error| {
                Error::local(format!("cannot reset the private clone stage: {error}"))
            })?;
        }
        Ok(())
    }

    /// A clone's task-owned sibling directory. Git never writes the caller's destination:
    /// after success `publish` renames this directory into an absent or still-empty destination.
    /// A concurrent file makes that rename fail without removing either the file or its parent.
    struct CloneStage {
        owner: aterm_tempfile::TempDir,
        destination: PathBuf,
    }

    impl CloneStage {
        fn for_op(op: &GitOp) -> Result<Option<CloneStage>> {
            let (GitOp::Clone { dest, .. } | GitOp::Mirror { dest }) = op else {
                return Ok(None);
            };
            let destination = std::path::absolute(dest)
                .map_err(|error| Error::local(format!("{}: {error}", dest.display())))?;
            let parent = destination.parent().ok_or_else(|| {
                Error::local(format!(
                    "{} has no parent for a private clone stage",
                    destination.display()
                ))
            })?;
            let owner = aterm_tempfile::Builder::new()
                .prefix(".agh-clone-")
                .suffix(".noindex")
                .tempdir_in(parent)
                .map_err(|error| {
                    Error::local(format!(
                        "cannot make a private clone stage beside {}: {error}",
                        destination.display()
                    ))
                })?;
            Ok(Some(CloneStage { owner, destination }))
        }

        fn path(&self) -> &Path {
            self.owner.path()
        }

        fn publish(self) -> Result<()> {
            std::fs::rename(self.path(), &self.destination).map_err(|error| {
                Error::local(format!(
                    "cannot publish the clone at {} without replacing caller files: {error}",
                    self.destination.display()
                ))
            })?;
            let _ = self.owner.keep();
            Ok(())
        }
    }

    /// The first absolute `PATH` entry of `env` holding an executable `git`, as `{entry}/git`.
    fn find_git(env: &dyn EnvSource) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        let path = env.var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .filter(|entry| entry.is_absolute())
            .map(|entry| entry.join("git"))
            .find(|git| {
                std::fs::metadata(git)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
            .ok_or_else(|| Error::local("git is not on PATH"))
    }

    /// `text` as one argument.
    fn os(text: impl Into<OsString>) -> OsString {
        text.into()
    }

    /// The run's one scratch directory, canonical, with `home/` and `work/` made.
    pub(super) fn scratch() -> Result<(aterm_tempfile::TempDir, PathBuf)> {
        let dir = aterm_tempfile::Builder::new()
            .prefix("agh-git-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|e| Error::local(format!("the git run's scratch directory: {e}")))?;
        let root = std::fs::canonicalize(dir.path())
            .map_err(|e| Error::local(format!("the git run's scratch directory: {e}")))?;
        for sub in ["home", "work"] {
            std::fs::create_dir(root.join(sub))
                .map_err(|e| Error::local(format!("the git run's scratch directory: {e}")))?;
        }
        Ok((dir, root))
    }

    /// The `GIT_CONFIG_COUNT` pairs, in order. `helper` is agh's helper, none for an anonymous
    /// run, which keeps only the empty `credential.helper` that resets every other one.
    fn config_pairs(helper: Option<&str>, remote: &Remote) -> Vec<(&'static str, String)> {
        std::iter::once(("credential.helper", String::new()))
            .chain(helper.map(|helper| ("credential.helper", helper.to_string())))
            .chain(config_rest())
            .chain(test_transport(remote))
            .collect()
    }

    /// The pairs after the helper's.
    fn config_rest() -> Vec<(&'static str, String)> {
        vec![
            ("credential.useHttpPath", "true".to_string()),
            ("http.followRedirects", "false".to_string()),
            ("http.sslVerify", "true".to_string()),
            ("protocol.allow", "never".to_string()),
            ("protocol.https.allow", "always".to_string()),
            ("core.askPass", String::new()),
            // Off for the hook directory only: git 2.54 also runs `hook.<name>.command` from
            // config, which this pair does not stop, so step 3's screen refuses `hook.*`.
            ("core.hooksPath", "/dev/null".to_string()),
            ("core.fsmonitor", "false".to_string()),
            // A fetch would otherwise start a detached `git maintenance run --auto`, which
            // outlives the run's bound and its scratch, holds the helper's environment, and can
            // fetch lazily from a remote the clone names. Command scope overrides the clone's.
            ("maintenance.auto", "false".to_string()),
        ]
    }

    /// `GIT_CEILING_DIRECTORIES`: the scratch, and for a fetch also the parent of its clone
    /// (canonical, as git compares it), so `git -C {dir}` looks at `dir` alone and never
    /// finds a repository that encloses it. Without that, a `dir` that is not a repository (an
    /// empty folder in a checkout, a home that is a dotfiles repository) would have the config
    /// screen read, and the fetch force-write the refs and tags of, the enclosing repository.
    fn ceiling(root: &Path, op: &GitOp) -> Result<OsString> {
        let (GitOp::CheckLocal { dir }
        | GitOp::CommitRead { dir, .. }
        | GitOp::Fetch { dir }
        | GitOp::FetchMain { dir, .. }
        | GitOp::FetchRef { dir, .. }
        | GitOp::FetchBatch { dir, .. }
        | GitOp::PushRefs { dir, .. }
        | GitOp::PushControl { dir, .. }) = op
        else {
            return Ok(root.as_os_str().to_os_string());
        };
        let dir = std::fs::canonicalize(dir)
            .or_else(|_| std::path::absolute(dir))
            .map_err(|e| Error::local(format!("{}: {e}", dir.display())))?;
        let Some(parent) = dir.parent() else {
            return Ok(root.as_os_str().to_os_string());
        };
        std::env::join_paths([root, parent]).map_err(|_| {
            Error::local(format!(
                "{}: a path holding ':' cannot bound git's search for the repository",
                dir.display()
            ))
        })
    }

    /// In a test or debug build, the one transport a test's remote needs turned back on:
    /// `protocol.http.allow` for a loopback URL, `protocol.file.allow` for a local remote.
    #[cfg(any(test, debug_assertions))]
    fn test_transport(remote: &Remote) -> Option<(&'static str, String)> {
        match remote {
            Remote::Loopback { .. } => Some(("protocol.http.allow", "always".to_string())),
            Remote::Local(_) => Some(("protocol.file.allow", "always".to_string())),
            Remote::Github(_) => None,
        }
    }

    /// A release build turns no transport back on.
    #[cfg(not(any(test, debug_assertions)))]
    const fn test_transport(_remote: &Remote) -> Option<(&'static str, String)> {
        None
    }

    /// Everything step 4 and step 5 put in a child's environment.
    struct EnvParts<'a> {
        gh: &'a Github,
        root: &'a Path,
        grant: Option<&'a str>,
        loose: bool,
        helper: Option<&'a str>,
        remote: &'a Remote,
        /// `GIT_CEILING_DIRECTORIES` (`ceiling`).
        ceiling: OsString,
    }

    /// The child's whole environment (steps 4 and 5).
    fn child_env(parts: &EnvParts<'_>) -> Vec<(OsString, OsString)> {
        let env = parts.gh.env();
        let mut out: Vec<(OsString, OsString)> = Vec::new();
        let mut set = |name: &str, value: OsString| out.push((OsString::from(name), value));
        for name in PASSED {
            if let Some(value) = env.var_os(name) {
                set(name, value);
            }
        }
        #[cfg(debug_assertions)]
        if let Some(value) = env.var_os("ATERM_STATE_HOME") {
            set("ATERM_STATE_HOME", value);
        }
        if let Some(nonce) = parts.grant {
            set(crate::git::GRANT_VAR, os(nonce));
        }
        if parts.loose {
            set(crate::git::LOOSE_VAR, os("1"));
        }
        set("HOME", parts.root.join("home").into_os_string());
        if let Some(dirs) = parts.gh.dirs() {
            set(
                crate::git::CONFIG_DIR_VAR,
                dirs.config.clone().into_os_string(),
            );
            set(
                crate::git::STATE_DIR_VAR,
                dirs.state.clone().into_os_string(),
            );
            set(crate::git::HOME_VAR, dirs.home.clone().into_os_string());
        }
        set("GIT_CEILING_DIRECTORIES", parts.ceiling.clone());
        #[cfg(any(test, debug_assertions))]
        if let Remote::Loopback { host, .. } = parts.remote {
            set(crate::git::LOOPBACK_VAR, os(host.as_str()));
        }
        set("GIT_CONFIG_GLOBAL", os("/dev/null"));
        set("GIT_CONFIG_NOSYSTEM", os("1"));
        set("GIT_TERMINAL_PROMPT", os("0"));
        set("GIT_NO_REPLACE_OBJECTS", os("1"));
        set("GIT_ASKPASS", OsString::new());
        let pairs = config_pairs(parts.helper, parts.remote);
        set("GIT_CONFIG_COUNT", os(pairs.len().to_string()));
        for (i, (key, value)) in pairs.into_iter().enumerate() {
            set(&format!("GIT_CONFIG_KEY_{i}"), os(key));
            set(&format!("GIT_CONFIG_VALUE_{i}"), os(value));
        }
        out
    }

    /// The run's own name for an op, in an exit's why.
    fn op_word(op: &GitOp) -> &'static str {
        match op {
            GitOp::CheckLocal { .. } => "check-local",
            GitOp::CommitRead { .. } => "commit-ancestry",
            GitOp::Clone { .. } | GitOp::Mirror { .. } => "clone",
            GitOp::Fetch { .. }
            | GitOp::FetchMain { .. }
            | GitOp::FetchRef { .. }
            | GitOp::FetchBatch { .. } => "fetch",
            GitOp::LsRemote { .. } => "ls-remote",
            GitOp::PushDelete { .. } | GitOp::PushRefs { .. } | GitOp::PushControl { .. } => "push",
        }
    }

    /// `bytes` as text, redacted whole and then cut to about its last `OUT_KEEP` bytes, at a
    /// char boundary. Redacting first means a token that straddles the cut is replaced whole,
    /// never left as a tail that neither the learned value nor a token shape matches.
    pub(super) fn kept(bytes: &[u8], redactor: &Redactor) -> String {
        let raw = String::from_utf8_lossy(bytes);
        let text = redactor.apply_cow(&raw);
        let from = text.len().saturating_sub(OUT_KEEP);
        if from == 0 {
            return text.into_owned();
        }
        let from = (from..=text.len())
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(text.len());
        text.get(from..).unwrap_or_default().to_string()
    }

    /// Step 3: whether a clone's local config key can change routing, trust, authentication or
    /// execution, or has HTTP behavior we have not audited. That includes a fetch's initial
    /// bundle URI (`fetch.bundleuri`, `bundle.*`) and a partial clone's promisor, fetched from
    /// lazily (`extensions.partialclone`, `remote.*.promisor`, `remote.*.partialclonefilter`),
    /// and a config-based hook (`hook.<name>.command`), which git 2.54 runs from the clone's
    /// config whatever `core.hooksPath` says.
    pub(super) fn rewrites(key: &str) -> bool {
        let (section, rest) = key.split_once('.').unwrap_or((key, ""));
        let (sub, var) = match rest.rsplit_once('.') {
            Some((sub, var)) => (Some(sub), var),
            None => (None, rest),
        };
        let is = |s: &str, list: &[&str]| list.iter().any(|w| s.eq_ignore_ascii_case(w));
        if section.eq_ignore_ascii_case("url") {
            sub.is_some() && is(var, &["insteadof", "pushinsteadof"])
        } else if section.eq_ignore_ascii_case("http") {
            // Git 2.54's urlmatch_config_entry strips a URL subsection at the last dot before
            // calling http_options, so these tuning variables preserve security in either scope.
            // Unknown variables stay refused. See design §6.9 and its linked per-key audit.
            !is(
                var,
                &[
                    "postbuffer",
                    "lowspeedlimit",
                    "lowspeedtime",
                    "maxrequests",
                    "minsessions",
                    "keepaliveidle",
                    "keepaliveinterval",
                    "keepalivecount",
                ],
            )
        } else if is(
            section,
            &[
                "credential",
                "include",
                "includeif",
                "protocol",
                "bundle",
                "hook",
            ],
        ) {
            true
        } else if section.eq_ignore_ascii_case("fetch") {
            sub.is_none() && var.eq_ignore_ascii_case("bundleuri")
        } else if section.eq_ignore_ascii_case("remote") {
            sub.is_some_and(|name| name.contains([':', '/']))
                || (sub.is_some()
                    && is(
                        var,
                        &[
                            "proxy",
                            "pushurl",
                            "uploadpack",
                            "receivepack",
                            "promisor",
                            "partialclonefilter",
                        ],
                    ))
        } else if section.eq_ignore_ascii_case("core") {
            sub.is_none()
                && is(
                    var,
                    &[
                        "sshcommand",
                        "gitproxy",
                        "fsmonitor",
                        "hookspath",
                        "alternaterefscommand",
                    ],
                )
        } else if section.eq_ignore_ascii_case("extensions") {
            sub.is_none() && var.eq_ignore_ascii_case("partialclone")
        } else {
            false
        }
    }

    /// Step 3's screen over each local/worktree `git config --list --name-only -z` output: a listing
    /// longer than `SCREEN_MAX`, or the first key outside the transport security policy, Refused.
    /// The child door keeps only the tail of a stream (`fx::child`, the last 1 MiB), so a listing
    /// past `SCREEN_MAX` may have lost its head, and a key cut off there would pass the screen
    /// while git still reads it: the screen fails closed on the length alone. Listing names
    /// only keeps a value from padding the listing; a long subsection name still can.
    pub(super) fn project_config_names(listed: &[u8], read_only: bool) -> Vec<u8> {
        if listed.len() > SCREEN_MAX {
            return aterm_json::to_vec(&ConfigVerdict::TooLong {
                bytes: listed.len(),
            })
            .unwrap_or_default();
        }
        let mut keys = 0;
        for entry in listed.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            keys += 1;
            let key = entry.split(|b| *b == b'\n').next().unwrap_or_default();
            let key = String::from_utf8_lossy(key);
            // Fetch passes its routed URL explicitly, never a named remote.
            // pushurl is write-only and cannot redirect these reads. Keep the
            // strict screen for writes and reject URL-shaped remote names.
            let write_only = key.split_once('.').is_some_and(|(section, rest)| {
                section.eq_ignore_ascii_case("remote")
                    && rest.rsplit_once('.').is_some_and(|(name, var)| {
                        !name.is_empty()
                            && !name.contains([':', '/'])
                            && var.eq_ignore_ascii_case("pushurl")
                    })
            });
            if rewrites(&key) && !(read_only && write_only) {
                return aterm_json::to_vec(&ConfigVerdict::Refused {
                    class: ConfigClass::of(&key),
                })
                .unwrap_or_default();
            }
        }
        aterm_json::to_vec(&ConfigVerdict::Allowed { keys }).unwrap_or_default()
    }

    fn screen_projected(projected: &[u8], slug: &Slug) -> Result<()> {
        let verdict: ConfigVerdict = aterm_json::from_slice(projected)
            .map_err(|_| Error::local("invalid isolated config-screen verdict"))?;
        match verdict {
            ConfigVerdict::Allowed { keys } if keys <= SCREEN_MAX => Ok(()),
            ConfigVerdict::Allowed { .. } => Err(Error::refused("invalid config-screen key count")),
            ConfigVerdict::TooLong { bytes } => Err(Error::refused(format!(
                "the clone's config is too long to screen ({bytes} bytes of names)"
            ))
            .with_repo(slug.to_string())),
            ConfigVerdict::Refused { class } => {
                let key = class.key().to_owned();
                Err(Error::refused(format!(
                    "the clone's config is outside agh's transport security policy: {key}"
                ))
                .with_sub(Sub::GitConfig { key })
                .with_next(
                    "remove the guarded key from the clone's local/worktree config, or clone again",
                )
                .with_repo(slug.to_string()))
            }
        }
    }

    /// Step 9: an exit that is not 0, mapped. Each phrase is looked for on stderr and stdout
    /// both, because `push --porcelain` reports a refused lease on stdout. `sent` is true only
    /// for the exit of the leased delete's own `git push`: an exit no phrase names there (a
    /// hung-up remote, an HTTP error, a signal agh did not send) may follow a delete GitHub
    /// applied, so it is could_not_run with `SideEffect::Unknown`, never read as not sent.
    /// Policy and unknown porcelain rejection reasons are Refused. Known receive
    /// infrastructure failures are CouldNotRun with an unknown side effect.
    pub(super) fn failed(
        op: &GitOp,
        out: &ChildOut,
        redactor: &Redactor,
        slug: &Slug,
        sent: bool,
    ) -> Error {
        let stderr = kept(&out.stderr, redactor);
        let stdout = kept(&out.stdout, redactor);
        let said = |needle: &str| stderr.contains(needle) || stdout.contains(needle);
        let last = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string();
        let error = if said("Authentication failed")
            || said("could not read Username")
            || said("told us to quit")
        {
            Error::new(
                Kind::Unauthenticated,
                format!("git {} was not let in: {last}", op_word(op)),
            )
        } else if said("stale info") {
            // `push --porcelain` reports a refused lease on stdout (`!\t(delete):{ref}\t
            // [rejected] (stale info)`), and stderr holds only git's generic line.
            let what = stdout
                .lines()
                .chain(stderr.lines())
                .find(|line| line.contains("stale info"))
                .map_or_else(|| last.clone(), |line| line.trim().to_string());
            Error::new(
                Kind::Conflict,
                "the ref no longer points at the expected sha".to_string(),
            )
            .with_sub(Sub::GuardFailed { what })
        } else if said("refusing to allow an OAuth App to create or update workflow") {
            Error::new(
                Kind::Forbidden,
                format!("git {} needs the workflow scope: {last}", op_word(op)),
            )
            .with_sub(Sub::Forbid(Forbid::Scope {
                needed: "workflow".to_string(),
            }))
        } else if let Some(line) = rejected(op, &stdout) {
            if server_rejection(&line) {
                Error::could_not_run(format!("GitHub receive side failed: {line}"))
                    .with_side_effect(if sent {
                        SideEffect::Unknown
                    } else {
                        SideEffect::None
                    })
            } else {
                Error::refused(format!("GitHub refused the push: {line}"))
            }
        } else {
            let code = out
                .code
                .map_or_else(|| "on a signal".to_string(), |code| code.to_string());
            let error = Error::could_not_run(format!("git {} exited {code}: {last}", op_word(op)))
                .with_sub(Sub::Git { exit: out.code });
            if sent
                && matches!(
                    op,
                    GitOp::PushDelete { .. }
                        | GitOp::PushRefs { dry_run: false, .. }
                        | GitOp::PushControl { dry_run: false, .. }
                )
            {
                error.with_side_effect(SideEffect::Unknown)
            } else {
                error
            }
        };
        error.with_repo(slug.to_string())
    }

    /// An authentication error from Git names the routed identity that supplied its helper
    /// credential. Git knows only that a helper answer failed; the route decision is the
    /// authoritative identity and must survive the child boundary in the diagnostic.
    fn name_routed_authentication(mut error: Error, profile: Option<&str>) -> Error {
        if error.kind == Kind::Unauthenticated
            && let Some(profile) = profile
        {
            error.why = format!(
                "routed identity {profile} failed authentication: {}",
                error.why
            );
        }
        error
    }

    /// A push's concrete porcelain rejection (stdout, already redacted), excluding
    /// the lease's `stale info`, which is Conflict.
    fn rejected(op: &GitOp, stdout: &str) -> Option<String> {
        if !matches!(
            op,
            GitOp::PushDelete { .. } | GitOp::PushRefs { .. } | GitOp::PushControl { .. }
        ) {
            return None;
        }
        let lines: Vec<_> = stdout
            .lines()
            .map(str::trim)
            .filter(|line| {
                line.starts_with('!')
                    && (line.contains("[rejected]") || line.contains("[remote rejected]"))
                    && !line.contains("stale info")
            })
            .collect();
        // Atomic pushes report a generic failure on otherwise valid sibling
        // refs. Preserve a concrete policy refusal even if a server failure
        // appears first, and use the concrete server reason over a sibling.
        lines
            .iter()
            .copied()
            .find(|line| {
                !server_rejection(line)
                    && !line.ends_with("[remote rejected] (atomic push failure)")
            })
            .or_else(|| lines.iter().copied().find(|line| server_rejection(line)))
            .or_else(|| lines.first().copied())
            .map(str::to_string)
    }

    /// Exact receive-pack infrastructure reasons; unknown and policy reasons
    /// remain refusals. `atomic push failure` alone is ambiguous (another row
    /// can have failed policy), so it is deliberately absent.
    fn server_rejection(line: &str) -> bool {
        let Some(reason) = line
            .split_once("[remote rejected] (")
            .and_then(|(_, reason)| reason.strip_suffix(')'))
        else {
            return false;
        };
        matches!(
            reason,
            "failed"
                | "failed to update ref"
                | "failed to update refs"
                | "transaction failed to start"
                | "atomic transaction failed"
                | "unable to migrate objects to permanent storage"
        )
    }

    /// Refused: git's credential helper, which routes again from agh's directories, would not
    /// answer as `profile` (`HelperRoute::answers_as`, the rules that pick its profile, with
    /// `granted` when the run writes a grant naming `profile`), so a git run would end
    /// Unauthenticated, or speak as another profile than `GitOut.as_` names. That happens when
    /// the run's call learned a vault token's login and could not record it (the record not
    /// writable, or another call holding its lock), and when the client's router is not the
    /// one agh.toml gives the helper (agh.toml changed since the client read it, or a library
    /// client built its router elsewhere).
    fn helper_answers(
        gh: &Github,
        dirs: &Dirs,
        profile: &str,
        granted: bool,
        loose: bool,
        slug: &Slug,
    ) -> Result<()> {
        let route = credential::route_for(gh.env(), dirs, loose)
            .map_err(|error| error.with_repo(slug.to_string()))?;
        if route.answers_as(slug, profile, granted, gh.env()) {
            return Ok(());
        }
        let vault_row = route.adopted && crate::identity::vault::Slot::named(profile).is_some();
        let (why, next) = if vault_row {
            (
                format!(
                    "{profile}'s login is not in the vault adoption record, which git's credential helper reads"
                ),
                route
                    .next
                    .unwrap_or_else(|| "run the command again".to_string()),
            )
        } else {
            (
                format!(
                    "git's credential helper, which reads agh.toml again, does not answer {slug} as {profile}"
                ),
                "agh auth status".to_string(),
            )
        };
        Err(Error::refused(why)
            .with_next(next)
            .with_repo(slug.to_string()))
    }

    /// The run on unix: steps 1 to 9 of the runner (plan §4.6.13).
    pub(super) fn run(gh: &Github, run: &GitRun) -> Result<GitOut> {
        check_op(run)?;
        let slug = &run.slug;
        let router = gh.router();
        let push = matches!(
            run.op,
            GitOp::PushDelete { .. } | GitOp::PushRefs { .. } | GitOp::PushControl { .. }
        );
        let anonymous = matches!(
            run.op,
            GitOp::LsRemote {
                anonymous: true,
                ..
            } | GitOp::CheckLocal { .. }
                | GitOp::FetchRef {
                    anonymous: true,
                    ..
                }
                | GitOp::FetchBatch {
                    anonymous: true,
                    ..
                }
                | GitOp::CommitRead {
                    anonymous: true,
                    ..
                }
        );
        let release = router.tier_of(slug.owner()) == Tier::Release;
        if release && !push && !literal_for(&run.named, slug) {
            return Err(release_needs_literal(slug));
        }
        let target = RouteTarget::Repo(slug.clone());
        let req = if anonymous {
            RouteRequest::read(target).with_anonymous(true)
        } else if push {
            RouteRequest::write(target)
        } else if release {
            RouteRequest::writer_read(target)
        } else {
            RouteRequest::read(target)
        }
        .with_named(run.named.clone());
        let profile = match gh.fx().route(&req, &mut || gh.decide(&req))? {
            Decision::Send { .. } if anonymous => {
                return Err(
                    Error::refused("anonymous git fetch cannot receive credentials")
                        .with_repo(slug.to_string()),
                );
            }
            Decision::Send { profile, .. } => Some(profile),
            Decision::Anonymous { .. } if anonymous || !(push || release) => None,
            Decision::Anonymous { .. } => {
                return Err(Error::refused(format!(
                    "a git run on {slug} speaks as {}'s writer, and the decision was anonymous",
                    slug.owner()
                ))
                .with_repo(slug.to_string()));
            }
            Decision::Refuse(refusal) => return Err(refusal.into_error(Some(slug))),
        };
        let anonymous_read = !push && profile.is_none();
        if let Some(name) = profile.as_deref()
            && let Some(error) = not_a_token_file(gh, name, slug)?
        {
            return Err(error);
        }
        let needs_grant = push || (release && !anonymous);
        let now = if needs_grant {
            Some(gh.fx().wall_secs()?)
        } else {
            None
        };
        let remote = Remote::of(gh, slug)?;
        if gh.fx().mode().replays() {
            let children = Children {
                gh,
                git: PathBuf::from("git"),
                env: Vec::new(),
                work: None,
                scratch_git: None,
                budget: run.budget,
                progress_window: run.progress_window,
                reads: !push,
                anonymous_read,
                clone_stage: None,
            };
            let routed = profile.clone();
            return finish(&children, run, &remote, profile, None)
                .map_err(|error| name_routed_authentication(error, routed.as_deref()));
        }
        let dirs = match (needs_grant, gh.dirs()) {
            (false, _) => None,
            (true, Some(dirs)) => Some(dirs),
            (true, None) => {
                return Err(Error::local("a git grant needs agh's state directory"));
            }
        };
        let loose = profile
            .as_deref()
            .and_then(|name| router.profile(name))
            .is_some_and(crate::identity::Profile::legacy_loose);
        if let Some(name) = profile.as_deref() {
            // The helper reads agh's directories from `AGH_GIT_*`, set only from `gh.dirs()`;
            // without them it would route by another agh.toml, or none, and `GitOut.as_`
            // would name a profile that never answered.
            let Some(helper_dirs) = gh.dirs() else {
                return Err(Error::local(format!(
                    "a git run as {name} needs agh's directories, which git's credential helper reads"
                ))
                .with_repo(slug.to_string()));
            };
            helper_answers(gh, helper_dirs, name, needs_grant, loose, slug)?;
        }
        if let (Some(dirs), Some(now)) = (dirs, now) {
            grant::sweep(dirs, now);
        }
        let git = find_git(gh.env())?;
        // An anonymous run gets no helper of agh's: only the reset of every other one. Its
        // helper would route again by agh.toml (or, on a client with no `Dirs`, by whatever
        // agh.toml the child's `XDG_CONFIG_HOME` holds) and could answer a token for a run
        // whose route, and `GitOut.as_`, said nobody.
        let helper = match profile {
            Some(_) => Some(gh.git_helper()?.config_value()),
            None => None,
        };
        let (scratch, root) = scratch()?;
        let ceiling = ceiling(&root, &run.op)?;
        let guard = match (dirs, now, &profile) {
            (Some(dirs), Some(now), Some(writer)) => {
                let ttl = run.budget.min(GRANT_MAX);
                let grant = grant::write(dirs, slug, writer, now, ttl)?;
                Some(GrantGuard { dirs, grant })
            }
            _ => None,
        };
        let mut env = child_env(&EnvParts {
            gh,
            root: &root,
            grant: guard.as_ref().map(|g| g.grant.nonce.as_str()),
            loose,
            helper: helper.as_deref(),
            remote: &remote,
            ceiling,
        });
        if matches!(run.op, GitOp::CheckLocal { .. }) {
            env.push((os("GIT_OPTIONAL_LOCKS"), os("0")));
        }
        let clone_stage = CloneStage::for_op(&run.op)?;
        let children = Children {
            gh,
            git,
            env,
            work: Some(root.join("work")),
            scratch_git: (push
                || matches!(
                    run.op,
                    GitOp::FetchRef { .. } | GitOp::FetchBatch { .. } | GitOp::CommitRead { .. }
                ))
            .then(|| root.join("scratch.git")),
            budget: run.budget,
            progress_window: run.progress_window,
            reads: !push,
            anonymous_read,
            clone_stage: clone_stage.as_ref().map(|stage| stage.path().to_path_buf()),
        };
        let nonce = guard.as_ref().map(|g| g.grant.nonce.clone());
        let routed = profile.clone();
        let result = finish(&children, run, &remote, profile, nonce)
            .map_err(|error| name_routed_authentication(error, routed.as_deref()));
        drop(guard);
        drop(scratch);
        let out = result?;
        if let Some(stage) = clone_stage {
            stage
                .publish()
                .map_err(|error| error.with_repo(run.slug.to_string()))?;
        }
        Ok(out)
    }

    /// Step 3: snapshot owned config without entering source repository setup,
    /// then parse only those bounded bytes in the run's safe scratch directory.
    /// Git setup can follow includes before a --local/--worktree listing even
    /// with --no-includes, so those repository-scoped queries are not admission.
    fn screened(children: &Children<'_>, run: &GitRun, dir: &Path) -> Result<PathBuf> {
        let slug = &run.slug;
        let dir = std::path::absolute(dir)
            .map_err(|e| Error::local(format!("{}: {e}", dir.display())))?;
        let read_only = matches!(
            &run.op,
            GitOp::CheckLocal { .. }
                | GitOp::CommitRead { .. }
                | GitOp::Fetch { .. }
                | GitOp::FetchMain { .. }
                | GitOp::FetchRef { .. }
                | GitOp::FetchBatch { .. }
        );
        let mut source = None;
        let admitted =
            children
                .gh
                .fx()
                .fact("git-source-config-v2", &slug.to_string(), &mut || {
                    match super::super::source_config::SourceConfig::read(&dir, SCREEN_MAX) {
                        Ok(snapshot) => {
                            source = Some(snapshot);
                            aterm_json::to_string(&ConfigSourceVerdict::Ready).unwrap_or_default()
                        }
                        Err(super::super::source_config::ReadError::Missing) => {
                            aterm_json::to_string(&ConfigSourceVerdict::Missing).unwrap_or_default()
                        }
                        Err(super::super::source_config::ReadError::Refused(error)) => {
                            aterm_json::to_string(&ConfigSourceVerdict::Refused(error))
                                .unwrap_or_default()
                        }
                    }
                })?;
        let admitted: ConfigSourceVerdict = aterm_json::from_str(&admitted)
            .map_err(|_| Error::local("invalid source-config admission"))?;
        match admitted {
            ConfigSourceVerdict::Ready => {}
            ConfigSourceVerdict::Missing => {
                return Err(Error::local(
                    "source has no repository common configuration",
                ));
            }
            ConfigSourceVerdict::Refused(error) => {
                return Err(Error::refused(error).with_repo(slug.to_string()));
            }
        }
        // In replay the filesystem closure is not called and the child door
        // supplies its taped safe verdict. No raw config values enter a tape.
        let common = source
            .as_ref()
            .map_or(&[][..], |source| source.common.as_slice());
        let names = |input: &[u8], scope: &str| -> Result<()> {
            let listed = children
                .config(
                    ConfigQuery::Names { read_only },
                    input,
                    &config_names_request(slug, read_only, scope),
                )
                .map_err(unsent)?;
            if listed.code != Some(0) {
                return Err(Error::refused("source configuration cannot be parsed")
                    .with_repo(slug.to_string()));
            }
            screen_projected(&listed.stdout, slug)
        };
        names(common, "local")?;
        let enabled = children
            .config(
                ConfigQuery::WorktreeFlag,
                common,
                format!("{CONFIG_LIST} {slug} worktree-flag").as_bytes(),
            )
            .map_err(unsent)?;
        let enabled = match (enabled.code, enabled.stdout.as_slice()) {
            (Some(0), b"true\n") => true,
            (Some(0), b"false\n") | (Some(1), b"") => false,
            _ => {
                return Err(
                    Error::refused("source worktreeConfig is not a valid boolean")
                        .with_repo(slug.to_string()),
                );
            }
        };
        if enabled {
            let mut worktree = None;
            let admitted =
                children
                    .gh
                    .fx()
                    .fact("git-worktree-config-v2", &slug.to_string(), &mut || {
                        let Some(source) = &source else {
                            return "source config snapshot is unavailable".into();
                        };
                        match source.worktree() {
                            Ok(Some(bytes)) => {
                                worktree = Some(bytes);
                                "present".into()
                            }
                            Ok(None) => "absent".into(),
                            Err(error) => error,
                        }
                    })?;
            match admitted.as_str() {
                "present" => names(worktree.as_deref().unwrap_or_default(), "worktree")?,
                "absent" => {}
                _ => return Err(Error::refused(admitted).with_repo(slug.to_string())),
            }
        }
        Ok(dir)
    }

    /// Cache refs must not traverse a symlink in either the ref or reflog's parent
    /// chain. Git's `--no-deref` protects a symbolic ref, not a filesystem symlink.
    /// As with the config screen, the caller owns the repository during this run.
    fn cache_parents(common: &Path, into: &str) -> Result<()> {
        for (is_reflog, relative) in [
            (false, std::borrow::Cow::Borrowed(into)),
            (true, std::borrow::Cow::Owned(format!("logs/{into}"))),
        ] {
            let mut current = common.to_path_buf();
            let mut components = Path::new(relative.as_ref()).components().peekable();
            while let Some(component) = components.next() {
                let is_last = components.peek().is_none();
                current.push(component.as_os_str());
                match std::fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(Error::refused(
                            "cache ref or reflog traverses a filesystem symlink",
                        ));
                    }
                    Ok(metadata) if !is_last && !metadata.is_dir() => {
                        return Err(Error::refused(
                            "cache ref or reflog parent is not a directory",
                        ));
                    }
                    Ok(metadata) if is_last && !metadata.is_file() => {
                        return Err(Error::refused("cache ref or reflog is not a regular file"));
                    }
                    Ok(metadata) if is_last && is_reflog && metadata.nlink() != 1 => {
                        return Err(Error::refused("cache reflog has another hard link"));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) => return Err(Error::local(format!("cache ref parent: {error}"))),
                }
            }
        }
        Ok(())
    }

    /// `rev-parse` of the caller `dir`: its absolute common directory, then whether it is
    /// shallow, one line each ([`source_view`] reads the answer).
    fn source_view_args(dir: &Path) -> Vec<OsString> {
        vec![
            os("-C"),
            dir.as_os_str().to_os_string(),
            os("rev-parse"),
            os("--path-format=absolute"),
            os("--git-common-dir"),
            os("--is-shallow-repository"),
        ]
    }

    /// The caller's common directory and whether it is shallow, from [`source_view_args`]'s
    /// answer. A common directory holding a newline or a carriage return, or one that is not
    /// absolute, fails the run.
    fn source_view(stdout: &[u8]) -> Result<(&str, bool)> {
        std::str::from_utf8(stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .and_then(|text| text.split_once('\n'))
            .filter(|(common, _)| !common.contains('\r') && Path::new(common).is_absolute())
            .and_then(|(common, shallow)| match shallow {
                "false" => Some((common, false)),
                "true" => Some((common, true)),
                _ => None,
            })
            .ok_or_else(|| Error::local("git returned an invalid common directory"))
    }

    /// Lend a fetch's private scratch the caller's object directory, read-only, and nothing
    /// else: with it in `objects/info/alternates`, the network fetch offers the caller's ref
    /// tips as haves and receives only objects the caller lacks, instead of the whole history
    /// on every run. A source named by a raw object ID whose closure the caller already holds
    /// needs no transfer, and git then fills the scratch ref without asking the remote; a
    /// source named by a ref is always resolved by the remote. The credentialed fetch still
    /// runs in the scratch under the environment the runner built (an inherited
    /// `GIT_ALTERNATE_OBJECT_DIRECTORIES` never reaches it), with the scratch's own fresh
    /// config (so no caller `core.alternateRefsCommand` or `core.alternateRefsPrefixes`) and
    /// hooks off; git lists the borrowed tips with a `for-each-ref` that reads the caller's
    /// screened config, as the fetch's import and update already do, and sends nothing. What
    /// each fetch checks of the fetched object IDs, its import and its no-deref update still
    /// decide what is accepted, and the caller gains no alternates file. A shallow caller's
    /// tips do not stand for complete history, so its scratch starts cold. `common` is what
    /// [`source_view`] admitted: an absolute path begins with '/', never with the '"' or '#'
    /// git's alternates parser reads specially, and holds no line break.
    fn lend_objects(scratch: Option<&Path>, common: &str, shallow: bool) -> Result<()> {
        let Some(scratch) = scratch.filter(|_| !shallow) else {
            return Ok(());
        };
        std::fs::write(
            scratch.join("objects/info/alternates"),
            format!("{common}/objects\n"),
        )
        .map_err(|e| Error::local(format!("private object view: {e}")))
    }

    /// Fetch into private scratch first, then import objects with no destination
    /// ref. Only the final no-deref update can change the caller's cache ref.
    fn fetch_exact(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        dir: &Path,
        reference: &str,
        into: &str,
    ) -> Result<ChildOut> {
        let dir = screened(children, run, dir)?;
        let canonical = crate::fx::canon::git_request(run);
        let step = |name: &str, args: Vec<OsString>, cwd: Option<&Path>| -> Result<ChildOut> {
            let key = [canonical.as_slice(), b"\nstep ", name.as_bytes()].concat();
            let out = if name == "fetch" {
                children.remote_read(run, &args, cwd, &key, None)
            } else {
                children.git(&args, cwd, &key)
            }
            .map_err(unsent)?;
            if out.code != Some(0) {
                return Err(failed(
                    &run.op,
                    &out,
                    children.gh.redactor(),
                    &run.slug,
                    false,
                ));
            }
            Ok(out)
        };
        let view = step("common-dir", source_view_args(&dir), None)?;
        let (common, shallow) = source_view(&view.stdout)?;
        let check_parents =
            || -> Result<()> {
                let answer =
                    children.gh.fx().fact(
                        "cache-ref-parents",
                        into,
                        &mut || match cache_parents(Path::new(common), into) {
                            Ok(()) => String::new(),
                            Err(error) => error.why.clone(),
                        },
                    )?;
                if answer.is_empty() {
                    Ok(())
                } else {
                    Err(Error::refused(answer))
                }
            };
        check_parents()?;
        let scratch = children.scratch_git.as_deref();
        let mut init = vec![os("init"), os("--quiet"), os("--bare")];
        if let Some(path) = scratch {
            init.push(path.as_os_str().to_os_string());
        }
        step("init", init, None)?;
        lend_objects(scratch, common, shallow)?;
        step(
            "fetch",
            vec![
                os("fetch"),
                os("--quiet"),
                os("--no-tags"),
                os("--no-write-fetch-head"),
                os("--recurse-submodules=no"),
                os(remote.url()),
                os(format!("+{reference}:refs/agh/source")),
            ],
            scratch,
        )?;
        let oid = step(
            "oid",
            vec![os("rev-parse"), os("--verify"), os("refs/agh/source")],
            scratch,
        )?;
        let oid = std::str::from_utf8(&oid.stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|text| crate::path::is_sha(text))
            .ok_or_else(|| Error::local("git returned an invalid fetched object id"))?;
        // Only this generated private directory gets file transport. No user URL
        // or repository remote participates in the local object transfer.
        let source = scratch.map_or_else(|| PathBuf::from("scratch.git"), Path::to_path_buf);
        step(
            "objects",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("-c"),
                os("protocol.file.allow=always"),
                os("fetch"),
                os("--quiet"),
                os("--no-tags"),
                os("--no-write-fetch-head"),
                os("--recurse-submodules=no"),
                source.into_os_string(),
                os("refs/agh/source"),
            ],
            None,
        )?;
        check_parents()?;
        step(
            "update",
            vec![
                os("-C"),
                dir.into_os_string(),
                os("update-ref"),
                os("--no-deref"),
                os("--create-reflog"),
                os(into),
                os(oid),
            ],
            None,
        )
    }

    /// Fetch one explicit snapshot into scratch; validate every raw OID before
    /// importing objects and atomically comparing/updating the caller's cache refs.
    fn fetch_batch(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        dir: &Path,
        batch: &super::super::FetchBatch,
    ) -> Result<ChildOut> {
        use std::io::Read as _;
        const ZERO: &str = "0000000000000000000000000000000000000000";
        let dir = screened(children, run, dir)?;
        let canonical = crate::fx::canon::git_request(run);
        let batch_key = crate::model::Sha256Digest::of(&canonical).hex();
        let step = |name: &str,
                    args: Vec<OsString>,
                    cwd: Option<&Path>,
                    input: Option<&[u8]>|
         -> Result<ChildOut> {
            let key = [canonical.as_slice(), b"\nstep ", name.as_bytes()].concat();
            let out = match (name, input) {
                ("fetch", input) => children.remote_read(run, &args, cwd, &key, input),
                (_, Some(input)) => children.git_input(&args, cwd, &key, input),
                (_, None) => children.git(&args, cwd, &key),
            }
            .map_err(unsent)?;
            if out.code != Some(0) {
                return Err(failed(
                    &run.op,
                    &out,
                    children.gh.redactor(),
                    &run.slug,
                    false,
                ));
            }
            Ok(out)
        };
        let view = step("common-dir", source_view_args(&dir), None, None)?;
        let (common, shallow) = source_view(&view.stdout)?;
        let check_parents = || -> Result<()> {
            let answer =
                children
                    .gh
                    .fx()
                    .fact("batch-cache-ref-parents", &batch_key, &mut || {
                        for row in batch.rows() {
                            if let Err(error) = cache_parents(Path::new(common), &row.into) {
                                return error.why.clone();
                            }
                            match std::fs::File::open(Path::new(common).join(&row.into)) {
                                Ok(file) => {
                                    let mut prefix = Vec::new();
                                    if let Err(error) = file.take(5).read_to_end(&mut prefix) {
                                        return format!("cache ref read: {error}");
                                    }
                                    if prefix.starts_with(b"ref:") {
                                        return "batch cache destination is a symbolic ref"
                                            .to_string();
                                    }
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(error) => return format!("cache ref read: {error}"),
                            }
                        }
                        String::new()
                    })?;
            if answer.is_empty() {
                Ok(())
            } else {
                Err(Error::refused(answer))
            }
        };
        use std::fmt::Write as _;
        check_parents()?;
        let mut old_input = String::new();
        for (index, row) in batch.rows().iter().enumerate() {
            let _ = writeln!(old_input, "{} {index}", row.into);
        }
        let old = step(
            "old-values",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("cat-file"),
                os("--batch-check=%(objectname) %(rest)"),
            ],
            None,
            Some(old_input.as_bytes()),
        )?;
        let old =
            std::str::from_utf8(&old.stdout).map_err(|_| Error::local("invalid old cache refs"))?;
        let mut before = Vec::with_capacity(batch.rows().len());
        for (index, line) in old.lines().enumerate() {
            let Some(row) = batch.rows().get(index) else {
                return Err(Error::local("extra old cache ref"));
            };
            let mut columns = line.split_whitespace();
            let oid = match (columns.next(), columns.next(), columns.next()) {
                (Some(oid), Some(number), None)
                    if crate::path::is_sha(oid) && number.parse::<usize>().ok() == Some(index) =>
                {
                    oid
                }
                (Some(reference), Some("missing"), None) if reference == row.into => ZERO,
                _ => return Err(Error::local("old cache ref identity differs")),
            };
            before.push(oid);
        }
        if before.len() != batch.rows().len() {
            return Err(Error::local("incomplete old cache refs"));
        }
        let scratch = children.scratch_git.as_deref();
        let mut init = vec![os("init"), os("--quiet"), os("--bare")];
        if let Some(path) = scratch {
            init.push(path.as_os_str().to_os_string());
        }
        step("init", init, None, None)?;
        lend_objects(scratch, common, shallow)?;
        let mut fetch_input = String::new();
        for (index, row) in batch.rows().iter().enumerate() {
            let _ = writeln!(fetch_input, "+{}:refs/agh/batch/{index}", row.source);
        }
        step(
            "fetch",
            vec![
                os("fetch"),
                os("--quiet"),
                os("--no-tags"),
                os("--no-write-fetch-head"),
                os("--recurse-submodules=no"),
                os("--stdin"),
                os(remote.url()),
            ],
            scratch,
            Some(fetch_input.as_bytes()),
        )?;
        let fetched = step(
            "oids",
            vec![
                os("for-each-ref"),
                os("--format=%(refname) %(objectname)"),
                os("refs/agh/batch/"),
            ],
            scratch,
            None,
        )?;
        let fetched = std::str::from_utf8(&fetched.stdout)
            .map_err(|_| Error::local("invalid fetched refs"))?;
        let mut seen = std::collections::BTreeSet::new();
        for line in fetched.lines() {
            let (reference, oid) = line
                .split_once(' ')
                .ok_or_else(|| Error::local("malformed fetched ref"))?;
            let index = reference
                .strip_prefix("refs/agh/batch/")
                .and_then(|index| index.parse::<usize>().ok())
                .ok_or_else(|| Error::local("unexpected fetched ref"))?;
            let row = batch
                .rows()
                .get(index)
                .ok_or_else(|| Error::local("extra fetched ref"))?;
            if !seen.insert(index) || oid != row.expected_oid {
                return Err(Error::refused(
                    "fetched source differs from its expected raw object ID; cache refs unchanged",
                ));
            }
        }
        if seen.len() != batch.rows().len() {
            return Err(Error::local(
                "incomplete fetched snapshot; cache refs unchanged",
            ));
        }
        check_parents()?;
        let mut objects_input = String::new();
        for index in 0..batch.rows().len() {
            let _ = writeln!(objects_input, "refs/agh/batch/{index}");
        }
        let source = scratch.map_or_else(|| PathBuf::from("scratch.git"), Path::to_path_buf);
        step(
            "objects",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("-c"),
                os("protocol.file.allow=always"),
                os("fetch"),
                os("--quiet"),
                os("--no-tags"),
                os("--no-write-fetch-head"),
                os("--recurse-submodules=no"),
                os("--stdin"),
                source.into_os_string(),
            ],
            None,
            Some(objects_input.as_bytes()),
        )?;
        check_parents()?;
        let mut transaction = "start\noption no-deref\n".to_string();
        for (row, old) in batch.rows().iter().zip(before) {
            let _ = writeln!(
                transaction,
                "update {} {} {old}",
                row.into, row.expected_oid
            );
        }
        transaction.push_str("prepare\ncommit\n");
        step(
            "update",
            vec![
                os("-C"),
                dir.into_os_string(),
                os("update-ref"),
                os("--no-deref"),
                os("--create-reflog"),
                os("-m"),
                os("agh exact batch fetch"),
                os("--stdin"),
            ],
            None,
            Some(transaction.as_bytes()),
        )
    }

    enum NativeRefBatch<'a> {
        Ordinary(&'a crate::github::refs::AtomicRefBatch),
        Control {
            transaction: &'a crate::git::ControlRefTransaction,
            updates: Vec<crate::github::refs::RefUpdate>,
        },
    }

    impl NativeRefBatch<'_> {
        fn updates(&self) -> &[crate::github::refs::RefUpdate] {
            match self {
                Self::Ordinary(batch) => batch.updates(),
                Self::Control { updates, .. } => updates,
            }
        }

        fn control(&self) -> Option<&crate::git::ControlRefTransaction> {
            match self {
                Self::Ordinary(_) => None,
                Self::Control { transaction, .. } => Some(transaction),
            }
        }

        fn source_roots(&self) -> Vec<&str> {
            match self {
                Self::Ordinary(batch) => batch
                    .updates()
                    .iter()
                    .map(|row| row.after.as_str())
                    .collect(),
                Self::Control {
                    transaction,
                    updates,
                } => std::iter::once(transaction.owner_commit())
                    .chain(
                        updates
                            .iter()
                            .flat_map(|row| [row.before.as_str(), row.after.as_str()]),
                    )
                    .filter(|oid| !oid.bytes().all(|byte| byte == b'0'))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            }
        }

        fn guard_refs(&self) -> Vec<&str> {
            if let Some(control) = self.control() {
                let changes = control.changes();
                // Safe failed-session cleanup owns only its exact token. Its
                // lease may already be absent or belong to a later publisher.
                if changes.len() == 1
                    && changes[0].reference == crate::git::PUBLISHER_FENCE_REF
                    && changes[0].after.is_none()
                {
                    return vec![crate::git::PUBLISHER_FENCE_REF];
                }
                return vec![
                    crate::git::PUBLISHER_FENCE_REF,
                    crate::git::RELEASE_LEASE_REF,
                ];
            }
            self.updates()
                .iter()
                .map(|row| row.reference.as_str())
                .collect()
        }
    }

    /// Import exact source objects into a private repository, validate all old
    /// values and ancestry, then perform one atomic push with explicit leases.
    /// The caller owns its source repository throughout the config screen and
    /// import; no local refs, hooks, config or FETCH_HEAD are written there.
    fn push_refs(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        dir: &Path,
        batch: &crate::github::refs::AtomicRefBatch,
        dry_run: bool,
    ) -> Result<ChildOut> {
        push_native_refs(
            children,
            run,
            remote,
            dir,
            NativeRefBatch::Ordinary(batch),
            dry_run,
        )
    }

    fn push_control(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        dir: &Path,
        transaction: &crate::git::ControlRefTransaction,
        dry_run: bool,
    ) -> Result<ChildOut> {
        const ZERO: &str = "0000000000000000000000000000000000000000";
        let updates = transaction
            .changes()
            .iter()
            .map(|row| crate::github::refs::RefUpdate {
                reference: row.reference.clone(),
                before: row.before.clone().unwrap_or_else(|| ZERO.into()),
                after: row.after.clone().unwrap_or_else(|| ZERO.into()),
            })
            .collect();
        push_native_refs(
            children,
            run,
            remote,
            dir,
            NativeRefBatch::Control {
                transaction,
                updates,
            },
            dry_run,
        )
    }

    fn push_native_refs(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        dir: &Path,
        batch: NativeRefBatch<'_>,
        dry_run: bool,
    ) -> Result<ChildOut> {
        use std::collections::{BTreeMap, BTreeSet};
        const ZERO: &str = "0000000000000000000000000000000000000000";
        let dir = screened(children, run, dir)?;
        let canonical = crate::fx::canon::git_request(run);
        let step = |name: &str, args: Vec<OsString>, cwd: Option<&Path>| -> Result<ChildOut> {
            let key = [canonical.as_slice(), b"\nstep ", name.as_bytes()].concat();
            children.git(&args, cwd, &key).map_err(unsent)
        };
        let checked = |name: &str, args: Vec<OsString>, cwd: Option<&Path>| -> Result<ChildOut> {
            let out = step(name, args, cwd)?;
            if out.code != Some(0) {
                return Err(failed(
                    &run.op,
                    &out,
                    children.gh.redactor(),
                    &run.slug,
                    false,
                ));
            }
            Ok(out)
        };
        let checked_input = |name: &str, args: Vec<OsString>, input: &[u8]| -> Result<ChildOut> {
            let key = [canonical.as_slice(), b"\nstep ", name.as_bytes()].concat();
            let out = children
                .git_input(&args, children.scratch_git.as_deref(), &key, input)
                .map_err(unsent)?;
            if out.code != Some(0) {
                return Err(failed(
                    &run.op,
                    &out,
                    children.gh.redactor(),
                    &run.slug,
                    false,
                ));
            }
            Ok(out)
        };
        if batch.control().is_some() {
            let origin = checked(
                "control-source-origin",
                vec![
                    os("-C"),
                    dir.as_os_str().to_owned(),
                    os("remote"),
                    os("get-url"),
                    os("origin"),
                ],
                None,
            )?;
            let url = std::str::from_utf8(&origin.stdout)
                .ok()
                .and_then(|value| value.strip_suffix('\n'))
                .filter(|value| !value.contains(['\n', '\r']))
                .ok_or_else(|| Error::refused("control source origin has unsupported framing"))?;
            let slug = run.slug.to_string();
            let admitted = [
                format!("https://github.com/{slug}"),
                format!("git@github.com:{slug}"),
                format!("ssh://git@github.com/{slug}"),
                format!("git@gh-{}:{slug}", run.slug.owner()),
            ];
            if !admitted.iter().any(|base| {
                url.eq_ignore_ascii_case(base) || url.eq_ignore_ascii_case(&format!("{base}.git"))
            }) {
                return Err(Error::refused(
                    "control transaction source origin names another repository",
                ));
            }
        }
        let scratch = children.scratch_git.as_deref();
        let mut init = vec![os("init"), os("--quiet"), os("--bare"), os("--template=")];
        if let Some(path) = scratch {
            init.push(path.as_os_str().to_os_string());
        }
        checked("init", init, None)?;
        let shallow = checked(
            "shallow",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("rev-parse"),
                os("--is-shallow-repository"),
            ],
            None,
        )?;
        if shallow.stdout != b"false\n" {
            return Err(Error::refused(
                "native push requires a complete, non-shallow source repository",
            ));
        }
        // A newly created tag has no BEFORE of its own. Observe main too, so
        // a tag of an already published commit need not import its history.
        // This is remote authority, never origin/main or a source accelerator.
        let mut guards = batch.guard_refs();
        if !guards.contains(&"refs/heads/main") {
            guards.push("refs/heads/main");
        }
        let mut list = vec![os("ls-remote"), os("--refs"), os(remote.url())];
        list.extend(guards.iter().map(|reference| os(*reference)));
        let listed = checked("remote", list, scratch)?;
        let text = std::str::from_utf8(&listed.stdout)
            .map_err(|_| Error::protocol("git returned non-UTF-8 refs"))?;
        let mut found = BTreeMap::new();
        for line in text.lines() {
            let (oid, reference) = line
                .split_once('\t')
                .ok_or_else(|| Error::protocol("git returned malformed refs"))?;
            if !crate::path::is_sha(oid)
                || !guards.contains(&reference)
                || found.insert(reference, oid).is_some()
            {
                return Err(Error::protocol("git returned unexpected or duplicate refs"));
            }
        }
        for u in batch.updates() {
            if found.get(u.reference.as_str()).copied().unwrap_or(ZERO) != u.before {
                return Err(Error::new(
                    Kind::Conflict,
                    "an atomic push's expected ref state differs",
                )
                .with_repo(run.slug.to_string()));
            }
        }
        if let Some(control) = batch.control() {
            control.check_before(
                found.get(crate::git::RELEASE_LEASE_REF).copied(),
                found.get(crate::git::PUBLISHER_FENCE_REF).copied(),
            )?;
        }
        // Only freshly observed remote objects can exclude an old object
        // closure. Source refs, grafts, replacement objects and accelerator
        // files are never copied into this private read/write repository.
        let alternate = scratch.map(|p| p.join("objects/info/alternates"));
        let mut remote_main = None;
        let mut remote_owned = false;
        let incremental = batch.updates().iter().any(|u| u.before != ZERO);
        let mut commit_facts = BTreeMap::<String, bool>::new();
        {
            let common = checked(
                "source-common",
                vec![
                    os("-C"),
                    dir.as_os_str().to_owned(),
                    os("rev-parse"),
                    os("--path-format=absolute"),
                    os("--git-common-dir"),
                ],
                None,
            )?;
            let common = std::str::from_utf8(&common.stdout)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| !s.contains(['\n', '\r']) && Path::new(s).is_absolute())
                .ok_or_else(|| Error::refused("source common directory has unsupported framing"))?;
            if let Some(path) = &alternate {
                std::fs::write(path, format!("{common}/objects\n"))
                    .map_err(|e| Error::local(format!("private object view: {e}")))?;
            }
            if let Some(main) = found.get("refs/heads/main") {
                let available = step(
                    "main-present",
                    vec![os("cat-file"), os("-e"), os(*main)],
                    scratch,
                )?;
                if available.code == Some(0) {
                    remote_main = Some(*main);
                }
            }
            // Tags and canonical lease/fence transitions introduce no file
            // content when their commit is already held by the observed main.
            // Verify their exact raw objects and a short hashed parent witness,
            // rather than copying or re-proving that remote-owned history.
            if let Some(main) = remote_main
                && (batch.control().is_some() || batch.updates().iter().all(|u| u.before == ZERO))
            {
                remote_owned = true;
                let mut witnessed = BTreeSet::new();
                for (i, root) in batch.source_roots().iter().enumerate() {
                    let kind = checked(
                        &format!("root-kind-{i}"),
                        vec![os("cat-file"), os("-t"), os(*root)],
                        scratch,
                    )?;
                    commit_facts.insert((*root).to_owned(), kind.stdout == b"commit\n");
                    let target = if kind.stdout == b"commit\n" {
                        (*root).to_owned()
                    } else if kind.stdout == b"tag\n" {
                        let tag = checked(
                            &format!("root-tag-{i}"),
                            vec![os("cat-file"), os("tag"), os(*root)],
                            scratch,
                        )?;
                        let target = std::str::from_utf8(&tag.stdout).ok().and_then(|s| {
                            let mut lines = s.lines();
                            let id = lines.next()?.strip_prefix("object ")?;
                            (crate::path::is_sha(id) && lines.next()? == "type commit")
                                .then(|| id.to_owned())
                        });
                        let Some(target) = target.filter(|_| !tag.stdout_may_be_truncated()) else {
                            remote_owned = false;
                            break;
                        };
                        target
                    } else {
                        remote_owned = false;
                        break;
                    };
                    if witnessed.insert(target.clone())
                        && !ancestry::witnessed_local_path(children, run, &dir, &target, main)?
                    {
                        remote_owned = false;
                        break;
                    }
                }
            }
            if !incremental && !remote_owned {
                // A genuinely unpublished closure needs a complete strictly
                // checked import, not the bounded incremental ID inventory.
                // Its source alternate must not satisfy the fetch's admission.
                if let Some(path) = &alternate {
                    std::fs::remove_file(path)
                        .map_err(|e| Error::local(format!("private object view: {e}")))?;
                }
                let mut import = vec![
                    os("-c"),
                    os("protocol.file.allow=always"),
                    os("-c"),
                    os("fetch.fsckObjects=true"),
                    os("-c"),
                    os("transfer.fsckObjects=true"),
                    os("fetch"),
                    os("--quiet"),
                    os("--no-tags"),
                    os("--no-write-fetch-head"),
                    os("--recurse-submodules=no"),
                    dir.as_os_str().to_owned(),
                ];
                for (i, oid) in batch.source_roots().iter().enumerate() {
                    import.push(os(format!("{oid}:refs/agh/source/{i}")));
                }
                checked("objects", import, scratch)?;
            } else {
                let mut metadata_roots = String::new();
                for u in batch.updates() {
                    if u.after != ZERO {
                        metadata_roots.push_str(&u.after);
                        metadata_roots.push('\n');
                    }
                    if u.before != ZERO {
                        metadata_roots.push_str(&u.before);
                        metadata_roots.push('\n');
                    }
                }
                if let Some(control) = batch.control() {
                    metadata_roots.push_str(control.owner_commit());
                    metadata_roots.push('\n');
                }
                if let Some(main) = remote_main.filter(|_| !remote_owned) {
                    metadata_roots.push_str(main);
                    metadata_roots.push('\n');
                }
                let prefix = scratch
                    .map_or_else(|| PathBuf::from("scratch.git"), Path::to_path_buf)
                    .join("objects/pack/pack");
                let metadata = children.gh.fx().native_push_phase(
                    crate::git::NativePushPhase::MetadataPack,
                    || {
                        let mut args = vec![
                            os("-c"),
                            os("core.commitGraph=false"),
                            os("-c"),
                            os("pack.useBitmaps=false"),
                            os("pack-objects"),
                            os("--quiet"),
                            os("--threads=8"),
                            os("--no-use-bitmap-index"),
                        ];
                        if remote_owned {
                            args.push(os("--no-revs"));
                        } else {
                            args.extend([os("--revs"), os("--filter=blob:none")]);
                        }
                        args.push(prefix.as_os_str().to_owned());
                        checked_input("metadata-pack", args, metadata_roots.as_bytes())
                    },
                )?;
                let hash = std::str::from_utf8(&metadata.stdout)
                    .ok()
                    .and_then(|s| s.strip_suffix('\n'))
                    .filter(|s| crate::path::is_sha(s))
                    .ok_or_else(|| Error::protocol("metadata pack ID is malformed"))?;
                children.gh.fx().native_push_phase(
                    crate::git::NativePushPhase::MetadataVerify,
                    || {
                        checked(
                            "metadata-verify",
                            vec![
                                os("index-pack"),
                                os("--verify"),
                                os("--strict"),
                                os("--threads=8"),
                                prefix
                                    .with_file_name(format!("pack-{hash}.pack"))
                                    .into_os_string(),
                            ],
                            scratch,
                        )
                    },
                )?;
                if !remote_owned {
                    let mut args = vec![
                        os("-c"),
                        os("core.commitGraph=false"),
                        os("-c"),
                        os("pack.useBitmaps=false"),
                        os("fsck"),
                        os("--connectivity-only"),
                        os("--no-dangling"),
                        os("--no-reflogs"),
                        os("--no-progress"),
                    ];
                    args.extend(batch.source_roots().iter().map(|oid| os(*oid)));
                    // Connectivity-only does not hash every tree. The full metadata
                    // pack above independently verifies all commit/tree/tag bytes and
                    // their original index IDs before any old closure is excluded.
                    // This extra walk checks referenced blob presence. New blob bytes
                    // are checked below; old blob bytes are already held by BEFORE.
                    children
                        .gh
                        .fx()
                        .native_push_phase(crate::git::NativePushPhase::Connectivity, || {
                            checked("connectivity", args, scratch)
                        })?;
                }
            }
        }
        // The admitted private object view stays fixed through these queries.
        // Share successful facts, while deciding each reference's policy separately.
        let mut is_commit = |name: &str, oid: &str| -> Result<bool> {
            if let Some(commit) = commit_facts.get(oid) {
                return Ok(*commit);
            }
            let commit = checked(name, vec![os("cat-file"), os("-t"), os(oid)], scratch)?.stdout
                == b"commit\n";
            commit_facts.insert(oid.to_owned(), commit);
            Ok(commit)
        };
        if let Some(control) = batch.control() {
            if !is_commit("control-owner-kind", control.owner_commit())? {
                return Err(Error::refused("the control owner must be a source commit"));
            }
            let tokens: BTreeSet<_> = control
                .changes()
                .iter()
                .filter(|row| row.reference == crate::git::PUBLISHER_FENCE_REF)
                .flat_map(|row| row.before.iter().chain(&row.after))
                .collect();
            for (index, token) in tokens.iter().enumerate() {
                let kind = checked(
                    &format!("control-token-kind-{index}"),
                    vec![os("cat-file"), os("-t"), os(token.as_str())],
                    scratch,
                )?;
                if kind.stdout != b"tag\n" {
                    return Err(Error::refused(
                        "a publisher control token must be annotated",
                    ));
                }
                let body = checked(
                    &format!("control-token-body-{index}"),
                    vec![os("cat-file"), os("tag"), os(token.as_str())],
                    scratch,
                )?;
                if body.stdout_may_be_truncated() {
                    return Err(Error::refused(
                        "publisher control token exceeds bounded object admission",
                    ));
                }
                control.check_fence_header(&body.stdout)?;
            }
        } else {
            let mut ancestor_pairs = BTreeSet::new();
            for (i, u) in batch.updates().iter().enumerate() {
                let after_is_commit = is_commit(&format!("kind-{i}"), &u.after)?;
                if u.reference.starts_with("refs/heads/") && !after_is_commit {
                    return Err(Error::refused("a branch must point directly at a commit"));
                }
                if u.before == ZERO {
                    continue;
                }
                if u.reference.starts_with("refs/tags/") {
                    return Err(Error::refused("an existing tag is immutable"));
                }
                // Every mutable namespace advances through commit ancestry; an
                // existing non-commit custom ref is immutable as well.
                if !after_is_commit {
                    return Err(Error::refused(
                        "an existing ref can advance only to a descendant commit",
                    ));
                }
                if !is_commit(&format!("before-kind-{i}"), &u.before)? {
                    return Err(Error::refused("an existing non-commit ref is immutable"));
                }
                let pair = (u.before.as_str(), u.after.as_str());
                if ancestor_pairs.contains(&pair) {
                    continue;
                }
                let ancestry = step(
                    &format!("ancestry-{i}"),
                    vec![
                        os("-c"),
                        os("core.commitGraph=false"),
                        os("merge-base"),
                        os("--is-ancestor"),
                        os(&u.before),
                        os(&u.after),
                    ],
                    scratch,
                )?;
                match ancestry.code {
                    Some(0) => {
                        ancestor_pairs.insert(pair);
                    }
                    Some(1) => {
                        return Err(Error::refused(
                            "native push refuses a non-fast-forward update",
                        ));
                    }
                    _ => {
                        return Err(failed(
                            &run.op,
                            &ancestry,
                            children.gh.redactor(),
                            &run.slug,
                            false,
                        ));
                    }
                }
            }
        }
        if incremental && !remote_owned && batch.updates().iter().any(|row| row.after != ZERO) {
            let mut roots = String::new();
            for u in batch.updates().iter().filter(|row| row.after != ZERO) {
                roots.push_str(&u.after);
                roots.push('\n');
            }
            for u in batch.updates().iter().filter(|u| u.before != ZERO) {
                roots.push('^');
                roots.push_str(&u.before);
                roots.push('\n');
            }
            if let Some(main) = remote_main {
                roots.push('^');
                roots.push_str(main);
                roots.push('\n');
            }
            let expected = checked_input(
                "new-object-ids",
                vec![
                    os("-c"),
                    os("core.commitGraph=false"),
                    os("-c"),
                    os("pack.useBitmaps=false"),
                    os("rev-list"),
                    os("--objects"),
                    os("--no-object-names"),
                    os("--stdin"),
                ],
                roots.as_bytes(),
            )?;
            if expected.stdout_may_be_truncated() {
                return Err(Error::refused(
                    "incremental object inventory reaches the 1 MiB admission limit",
                ));
            }
            let ids = std::str::from_utf8(&expected.stdout)
                .map_err(|_| Error::protocol("new object IDs are not UTF-8"))?;
            if ids.lines().any(|s| !crate::path::is_sha(s)) {
                return Err(Error::protocol("new object IDs are malformed"));
            }
            let prefix = scratch
                .map_or_else(|| PathBuf::from("scratch.git"), Path::to_path_buf)
                .join("objects/pack/pack");
            let pack = checked_input(
                "new-object-pack",
                vec![
                    os("-c"),
                    os("core.commitGraph=false"),
                    os("-c"),
                    os("pack.useBitmaps=false"),
                    os("pack-objects"),
                    os("--quiet"),
                    os("--revs"),
                    os("--threads=8"),
                    os("--no-use-bitmap-index"),
                    prefix.as_os_str().to_owned(),
                ],
                roots.as_bytes(),
            )?;
            let hash = std::str::from_utf8(&pack.stdout)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| crate::path::is_sha(s))
                .ok_or_else(|| Error::protocol("new object pack ID is malformed"))?;
            let path = prefix.with_file_name(format!("pack-{hash}.pack"));
            checked(
                "new-object-verify",
                vec![
                    os("index-pack"),
                    os("--verify"),
                    os("--strict"),
                    os("--threads=8"),
                    path.into_os_string(),
                ],
                scratch,
            )?;
            // Do not let the source alternate satisfy new-object membership:
            // every newly needed ID must reside in the strictly verified pack.
            let borrowed = if let Some(path) = &alternate {
                let bytes = std::fs::read(path)
                    .map_err(|e| Error::local(format!("private object view: {e}")))?;
                std::fs::remove_file(path)
                    .map_err(|e| Error::local(format!("private object view: {e}")))?;
                Some(bytes)
            } else {
                None
            };
            let owned = checked_input(
                "new-object-membership",
                vec![
                    os("cat-file"),
                    os("--batch-check=%(objectname) %(objecttype)"),
                ],
                ids.as_bytes(),
            )?;
            if owned.stdout_may_be_truncated() {
                return Err(Error::refused(
                    "incremental object membership reaches the 1 MiB admission limit",
                ));
            }
            let owned = std::str::from_utf8(&owned.stdout)
                .map_err(|_| Error::protocol("owned object IDs are not UTF-8"))?;
            if owned.lines().count() != ids.lines().count()
                || !owned.lines().zip(ids.lines()).all(|(line, oid)| {
                    line.split_once(' ').is_some_and(|(found, kind)| {
                        found == oid && matches!(kind, "commit" | "tree" | "blob" | "tag")
                    })
                })
            {
                return Err(Error::refused(
                    "new objects are absent from the strictly verified owned pack",
                ));
            }
            if let (Some(path), Some(bytes)) = (&alternate, borrowed) {
                std::fs::write(path, bytes)
                    .map_err(|e| Error::local(format!("private object view: {e}")))?;
            }
        }
        let mut final_list = vec![os("ls-remote"), os("--refs"), os(remote.url())];
        final_list.extend(guards.iter().map(|reference| os(*reference)));
        if checked("remote-final", final_list, scratch)?.stdout != listed.stdout {
            return Err(Error::new(
                Kind::Conflict,
                "the remote ref changed during push preparation",
            ));
        }
        let mut args = vec![
            os("-c"),
            os("core.commitGraph=false"),
            os("-c"),
            os("pack.useBitmaps=false"),
            os("-c"),
            os("pack.threads=8"),
            os("push"),
            os("--atomic"),
            os("--porcelain"),
            os("--no-follow-tags"),
            os("--recurse-submodules=no"),
        ];
        if dry_run {
            args.push(os("--dry-run"));
        }
        for u in batch.updates() {
            let before = if u.before == ZERO { "" } else { &u.before };
            args.push(os(format!("--force-with-lease={}:{before}", u.reference)));
        }
        args.push(os(remote.url()));
        args.extend(batch.updates().iter().map(|u| {
            if u.after == ZERO {
                os(format!(":{}", u.reference))
            } else {
                os(format!("{}:{}", u.after, u.reference))
            }
        }));
        let started = children.gh.fx().mono()?;
        let mut pause = std::time::Duration::from_secs(1);
        let out = loop {
            let out = children
                .git(&args, scratch, &canonical)
                .map_err(|e| if dry_run { unsent(e) } else { e })?;
            if out.code == Some(0) {
                break out;
            }
            let error = failed(&run.op, &out, children.gh.redactor(), &run.slug, !dry_run);
            if dry_run
                || error.kind != Kind::CouldNotRun
                || error.side_effect != SideEffect::Unknown
            {
                return Err(error);
            }
            // A receive failure says nothing about whether the CAS committed.
            // Read all guarded refs, including the control refs we do not change.
            let mut list = vec![os("ls-remote"), os("--refs"), os(remote.url())];
            list.extend(guards.iter().map(|reference| os(*reference)));
            let read = checked("remote-recover", list, scratch).map_err(|read| {
                Error::could_not_run(format!(
                    "{}; remote re-read failed: {}",
                    error.why, read.why
                ))
                .with_side_effect(SideEffect::Unknown)
            })?;
            if read.stdout_may_be_truncated() {
                return Err(Error::protocol(
                    "remote recovery refs exceed the complete output bound",
                )
                .with_side_effect(SideEffect::Unknown));
            }
            let text = std::str::from_utf8(&read.stdout).map_err(|_| {
                Error::protocol("git returned non-UTF-8 refs").with_side_effect(SideEffect::Unknown)
            })?;
            let mut observed = BTreeMap::new();
            for line in text.lines() {
                let (oid, reference) = line.split_once('\t').ok_or_else(|| {
                    Error::protocol("git returned malformed refs")
                        .with_side_effect(SideEffect::Unknown)
                })?;
                if !crate::path::is_sha(oid)
                    || !guards.contains(&reference)
                    || observed.insert(reference, oid).is_some()
                {
                    return Err(Error::protocol("git returned unexpected or duplicate refs")
                        .with_side_effect(SideEffect::Unknown));
                }
            }
            if batch
                .updates()
                .iter()
                .all(|u| observed.get(u.reference.as_str()).copied().unwrap_or(ZERO) == u.after)
            {
                return Ok(ChildOut {
                    code: Some(0),
                    stdout: b"remote re-read confirmed all requested refs\n".to_vec(),
                    stderr: Vec::new(),
                });
            }
            if observed != found {
                let partial = batch.updates().iter().any(|u| {
                    observed.get(u.reference.as_str()).copied().unwrap_or(ZERO) == u.after
                });
                return Err(Error::new(
                    Kind::Conflict,
                    "remote refs changed after an uncertain push",
                )
                .with_side_effect(if partial {
                    SideEffect::Unknown
                } else {
                    SideEffect::None
                }));
            }
            // No ref progress: use elapsed patience, never an attempt count.
            // Repeated healthy reads of BEFORE are not progress and cannot
            // keep a broken receive service alive forever.
            if children.gh.fx().mono()?.saturating_sub(started) >= run.budget {
                return Err(
                    Error::could_not_run(format!("no remote ref progress: {}", error.why))
                        .with_side_effect(SideEffect::None),
                );
            }
            children.gh.fx().sleep(pause)?;
            pause = (pause * 2).min(std::time::Duration::from_secs(30));
        };
        // Git omits a lease when its advertisement already equals AFTER. That
        // can race our earlier read even when BEFORE != AFTER in every input.
        // Other rows may have landed, so never call that a complete CAS success.
        let mut acknowledged = std::collections::BTreeSet::new();
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let mut fields = line.split('\t');
            let (Some(flag), Some(spec), Some(_summary)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some((source, reference)) = spec.split_once(':') else {
                continue;
            };
            let matches = batch.updates().iter().any(|update| {
                if update.reference != reference {
                    return false;
                }
                if batch.control().is_none() {
                    return matches!(flag, " " | "*") && update.after == source;
                }
                if update.after == ZERO {
                    return flag == "-" && matches!(source, "" | "(delete)");
                }
                let acknowledged = if update.before == ZERO {
                    flag == "*"
                } else {
                    // Only the validated annotated control token can rotate.
                    update.reference == crate::git::PUBLISHER_FENCE_REF && matches!(flag, " " | "+")
                };
                acknowledged && update.after == source
            });
            if !matches || !acknowledged.insert(reference) {
                return Err(Error::new(Kind::OutcomeUnknown, "native push did not acknowledge every explicit lease; concurrent changes or partial effects are possible")
                    .with_side_effect(if dry_run { SideEffect::None } else { SideEffect::Unknown }).with_repo(run.slug.to_string()));
            }
        }
        if acknowledged.len() != batch.updates().len() {
            return Err(Error::new(
                Kind::OutcomeUnknown,
                "native push has missing per-ref acknowledgements",
            )
            .with_side_effect(if dry_run {
                SideEffect::None
            } else {
                SideEffect::Unknown
            })
            .with_repo(run.slug.to_string()));
        }
        Ok(out)
    }

    /// Steps 3, 7 and 9: the config screen of a fetch, the children, and the exit's mapping.
    fn finish(
        children: &Children<'_>,
        run: &GitRun,
        remote: &Remote,
        profile: Option<String>,
        grant: Option<String>,
    ) -> Result<GitOut> {
        let slug = &run.slug;
        let redactor = children.gh.redactor();
        let url = os(remote.url());
        let (args, cwd): (Vec<OsString>, Option<&Path>) = match &run.op {
            GitOp::CommitRead {
                dir,
                request,
                anonymous,
            } => {
                let out =
                    ancestry::read(children, run, dir, request, profile.as_deref(), *anonymous)?;
                if out.stdout.len() > 256 * 1024 {
                    return Err(Error::protocol(
                        "native commit observation exceeds its complete output bound",
                    ));
                }
                return Ok(GitOut {
                    stdout: redactor
                        .apply_cow(std::str::from_utf8(&out.stdout).map_err(|_| {
                            Error::protocol("native commit observation is not UTF8")
                        })?)
                        .into_owned(),
                    stderr: kept(&out.stderr, redactor),
                    as_: profile,
                    grant,
                });
            }
            GitOp::Mirror { dest } => {
                let dest = match children.clone_stage.as_ref() {
                    Some(stage) => stage.clone(),
                    None => std::path::absolute(dest)
                        .map_err(|e| Error::local(format!("{}: {e}", dest.display())))?,
                };
                (
                    vec![
                        os("clone"),
                        os("--quiet"),
                        os("--mirror"),
                        os("--"),
                        url,
                        dest.into_os_string(),
                    ],
                    None,
                )
            }
            GitOp::Clone { dest, depth } => {
                let dest = match children.clone_stage.as_ref() {
                    Some(stage) => stage.clone(),
                    None => std::path::absolute(dest)
                        .map_err(|e| Error::local(format!("{}: {e}", dest.display())))?,
                };
                let mut args = vec![os("clone"), os("--quiet")];
                if let Some(depth) = depth {
                    args.push(os(format!("--depth={depth}")));
                }
                args.extend([os("--"), url, dest.into_os_string()]);
                (args, None)
            }
            GitOp::CheckLocal { dir } => {
                let dir = screened(children, run, dir)?;
                let args = vec![
                    os("-C"),
                    dir.into_os_string(),
                    os("rev-parse"),
                    os("--path-format=absolute"),
                    os("--git-common-dir"),
                ];
                (args, None)
            }
            GitOp::Fetch { dir } => {
                let dir = screened(children, run, dir)?;
                let args = vec![
                    os("-C"),
                    dir.into_os_string(),
                    os("fetch"),
                    os("--quiet"),
                    os("--tags"),
                    os("--force"),
                    os("--recurse-submodules=no"),
                    url,
                    os("+refs/heads/*:refs/remotes/origin/*"),
                ];
                (args, None)
            }
            GitOp::FetchMain { dir, into } => {
                let dir = screened(children, run, dir)?;
                let args = vec![
                    os("-C"),
                    dir.into_os_string(),
                    os("-c"),
                    os("core.logAllRefUpdates=always"),
                    os("fetch"),
                    os("--quiet"),
                    os("--no-tags"),
                    os("--no-write-fetch-head"),
                    os("--recurse-submodules=no"),
                    url,
                    os(format!("+refs/heads/main:{into}")),
                ];
                (args, None)
            }
            GitOp::FetchRef {
                dir,
                reference,
                into,
                ..
            } => {
                let out = fetch_exact(children, run, remote, dir, reference, into)?;
                return Ok(GitOut {
                    stdout: kept(&out.stdout, redactor),
                    stderr: kept(&out.stderr, redactor),
                    as_: profile,
                    grant,
                });
            }
            GitOp::PushRefs {
                dir,
                batch,
                dry_run,
            } => {
                let out = push_refs(children, run, remote, dir, batch, *dry_run)?;
                return Ok(GitOut {
                    stdout: kept(&out.stdout, redactor),
                    stderr: kept(&out.stderr, redactor),
                    as_: profile,
                    grant,
                });
            }
            GitOp::PushControl {
                dir,
                transaction,
                dry_run,
            } => {
                let out = push_control(children, run, remote, dir, transaction, *dry_run)?;
                return Ok(GitOut {
                    stdout: kept(&out.stdout, redactor),
                    stderr: kept(&out.stderr, redactor),
                    as_: profile,
                    grant,
                });
            }
            GitOp::FetchBatch { dir, batch, .. } => {
                let out = fetch_batch(children, run, remote, dir, batch)?;
                return Ok(GitOut {
                    stdout: kept(&out.stdout, redactor),
                    stderr: kept(&out.stderr, redactor),
                    as_: profile,
                    grant,
                });
            }
            GitOp::LsRemote {
                reference,
                anonymous,
            } => {
                let mut args = vec![os("ls-remote")];
                if !anonymous && reference.as_deref() != Some("HEAD") {
                    args.push(os("--refs"));
                }
                args.push(url);
                if let Some(reference) = reference {
                    args.push(os(reference.as_str()));
                }
                (args, None)
            }
            GitOp::PushDelete { reference, expect } => {
                let scratch_git = children.scratch_git.as_deref();
                let mut init = vec![os("init"), os("--quiet"), os("--bare")];
                if let Some(path) = scratch_git {
                    init.push(path.as_os_str().to_os_string());
                }
                let made = children
                    .git(&init, None, INIT_SCRATCH.as_bytes())
                    .map_err(unsent)?;
                if made.code != Some(0) {
                    return Err(failed(&run.op, &made, redactor, slug, false));
                }
                let args = vec![
                    os("push"),
                    os("--porcelain"),
                    os(format!("--force-with-lease={reference}:{expect}")),
                    url,
                    os(format!(":{reference}")),
                ];
                (args, scratch_git)
            }
        };
        let canonical = crate::fx::canon::git_request(run);
        let started = children.gh.fx().mono()?;
        let mut pause = std::time::Duration::from_secs(1);
        let out = loop {
            let out = if matches!(
                run.op,
                GitOp::Clone { .. }
                    | GitOp::Mirror { .. }
                    | GitOp::Fetch { .. }
                    | GitOp::FetchMain { .. }
                    | GitOp::LsRemote { .. }
            ) {
                children.remote_read(run, &args, cwd, &canonical, None)?
            } else {
                children.git(&args, cwd, &canonical)?
            };
            if out.code == Some(0) {
                break out;
            }
            let error = failed(&run.op, &out, redactor, slug, true);
            let GitOp::PushDelete { reference, expect } = &run.op else {
                return Err(error);
            };
            if error.kind != Kind::CouldNotRun || error.side_effect != SideEffect::Unknown {
                return Err(error);
            }
            let read_key = [canonical.as_slice(), b"\nstep delete-recover"].concat();
            let read = children
                .git(
                    &[
                        os("ls-remote"),
                        os("--refs"),
                        os(remote.url()),
                        os(reference),
                    ],
                    cwd,
                    &read_key,
                )
                .map_err(|read| {
                    Error::could_not_run(format!(
                        "{}; remote re-read failed: {}",
                        error.why, read.why
                    ))
                    .with_side_effect(SideEffect::Unknown)
                })?;
            if read.code != Some(0) {
                return Err(Error::could_not_run(format!(
                    "{}; remote re-read failed: {}",
                    error.why,
                    kept(&read.stderr, redactor)
                ))
                .with_side_effect(SideEffect::Unknown));
            }
            if read.stdout_may_be_truncated() {
                return Err(Error::protocol(
                    "remote recovery refs exceed the complete output bound",
                )
                .with_side_effect(SideEffect::Unknown));
            }
            if read.stdout.is_empty() {
                break ChildOut {
                    code: Some(0),
                    stdout: b"remote re-read confirmed ref absence\n".to_vec(),
                    stderr: Vec::new(),
                };
            }
            let expected = format!("{expect}\t{reference}\n");
            if read.stdout != expected.as_bytes() {
                return Err(Error::new(
                    Kind::Conflict,
                    "remote ref changed after an uncertain delete",
                )
                .with_side_effect(SideEffect::None));
            }
            if children.gh.fx().mono()?.saturating_sub(started) >= run.budget {
                return Err(
                    Error::could_not_run(format!("no remote ref progress: {}", error.why))
                        .with_side_effect(SideEffect::None),
                );
            }
            children.gh.fx().sleep(pause)?;
            pause = (pause * 2).min(std::time::Duration::from_secs(30));
        };
        if matches!(
            run.op,
            GitOp::LsRemote {
                anonymous: true,
                ..
            }
        ) && out.stdout.len() > 16 * 1024 * 1024
        {
            return Err(Error::refused(
                "remote ref advertisement exceeds the complete output bound",
            )
            .with_repo(slug.to_string()));
        }
        Ok(GitOut {
            stdout: if matches!(
                run.op,
                GitOp::LsRemote {
                    anonymous: true,
                    ..
                }
            ) {
                redactor.apply(&String::from_utf8_lossy(&out.stdout))
            } else {
                kept(&out.stdout, redactor)
            },
            stderr: kept(&out.stderr, redactor),
            as_: profile,
            grant,
        })
    }

    #[cfg(test)]
    mod config_replay_tests {
        use super::*;

        #[test]
        fn read_only_config_projection_cannot_replay_as_write_admission() {
            use crate::fx::tape::{TapeHeader, TapeMode};

            let root = aterm_tempfile::Builder::new()
                .prefix("agh-config-replay-")
                .tempdir()
                .unwrap();
            let tape = root.path().join("config.tape");
            let slug = Slug::new("fixture-dev", "app").unwrap();
            let input = b"[remote \"origin\"]\npushurl = https://elsewhere.invalid/app\n";
            let query = ConfigQuery::Names { read_only: true };
            let mut spec = ChildSpec::new(&find_git(&crate::identity::ProcessEnv).unwrap())
                .cwd(root.path())
                .config_input(input, query);
            for arg in query.args() {
                spec = spec.arg(*arg);
            }
            let mut header = TapeHeader::for_run(
                &["config-screen".into()],
                &crate::wire::Endpoints::github_com(),
            );
            header.mode = TapeMode::Credentialed;
            let recording = crate::fx::Fx::record_for_tests(&tape, &header).unwrap();
            let answer = recording
                .git(&spec, &config_names_request(&slug, true, "local"))
                .unwrap();
            assert_eq!(answer.code, Some(0));
            screen_projected(&answer.stdout, &slug).unwrap();
            recording.finish(0).unwrap();
            let (replay, _) = crate::fx::Fx::replay_for_tests(&tape).unwrap();
            let write = spec.config_input(input, ConfigQuery::Names { read_only: false });
            replay
                .git(&write, &config_names_request(&slug, false, "local"))
                .expect_err("read-only pushurl permission must not become write permission");
            assert!(
                replay.finish(0).is_err(),
                "the semantic divergence must stay latched"
            );
        }

        #[test]
        fn config_verdict_rejects_unknown_protocol_fields() {
            let slug = Slug::new("fixture-dev", "app").unwrap();
            assert!(screen_projected(br#"{"Allowed":{"keys":1,"extra":true}}"#, &slug).is_err());
        }
    }

    #[cfg(test)]
    mod transport_retry_tests {
        use super::*;

        #[test]
        fn private_header_trace_recovers_numeric_retry_after_then_is_removed() {
            let root = aterm_tempfile::Builder::new()
                .prefix("agh-git-trace-")
                .tempdir()
                .unwrap();
            let trace = root.path().join("curl.trace");
            std::fs::write(
                &trace,
                b"00:00:00 <= Recv header: HTTP/2 429\r\n\
                  00:00:00 <= Recv header: Retry-After: 7\r\n",
            )
            .unwrap();
            assert_eq!(
                git_trace_observation(&trace, None),
                GitObservation::from_trace(
                    "00:00:00 <= Recv header: HTTP/2 429\r\n\
                     00:00:00 <= Recv header: Retry-After: 7\r\n"
                )
            );
            assert!(!trace.exists());
        }

        #[test]
        fn private_header_trace_uses_the_run_clock_for_http_date_retry_after() {
            let root = aterm_tempfile::Builder::new()
                .prefix("agh-git-trace-date-")
                .tempdir()
                .unwrap();
            let trace = root.path().join("curl.trace");
            let text = "00:00:00 <= Recv header: HTTP/2 429\r\n\
                        00:00:00 <= Recv header: Retry-After: Sun, 08 Nov 2026 12:00:00 GMT\r\n";
            std::fs::write(&trace, text).unwrap();
            let now = 1_762_600_000;

            assert_eq!(
                git_trace_observation(&trace, Some(now)),
                GitObservation::from_trace_at(text, Some(now))
            );
            assert!(!trace.exists());
        }

        /// NEGATIVE CONTROL: publishing directly into `destination`, or clearing it after a
        /// failed attempt, removes `caller's-file` and fails this assertion.
        #[test]
        fn clone_stage_refuses_to_replace_a_concurrent_destination_file() {
            let root = aterm_tempfile::Builder::new()
                .prefix("agh-clone-stage-")
                .tempdir()
                .unwrap();
            let destination = root.path().join("destination");
            let stage = CloneStage::for_op(&GitOp::Clone {
                dest: destination.clone(),
                depth: None,
            })
            .unwrap()
            .unwrap();
            let stage_path = stage.path().to_path_buf();
            std::fs::write(stage.path().join("downloaded"), b"network bytes").unwrap();

            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("caller's-file"), b"keep").unwrap();
            let error = stage
                .publish()
                .expect_err("a nonempty destination wins the publish race");

            assert!(error.why.contains("without replacing caller files"));
            assert_eq!(
                std::fs::read(destination.join("caller's-file")).unwrap(),
                b"keep"
            );
            assert!(!destination.join("downloaded").exists());
            assert!(!stage_path.exists(), "only the task-owned stage is cleaned");
        }

        #[test]
        fn clone_stage_publishes_over_the_still_empty_admitted_destination() {
            let root = aterm_tempfile::Builder::new()
                .prefix("agh-clone-publish-")
                .tempdir()
                .unwrap();
            let destination = root.path().join("destination");
            std::fs::create_dir(&destination).unwrap();
            let stage = CloneStage::for_op(&GitOp::Clone {
                dest: destination.clone(),
                depth: None,
            })
            .unwrap()
            .unwrap();
            std::fs::write(stage.path().join("downloaded"), b"complete").unwrap();

            stage.publish().unwrap();

            assert_eq!(
                std::fs::read(destination.join("downloaded")).unwrap(),
                b"complete"
            );
        }

        /// NEGATIVE CONTROL: passing the caller destination to the retry cleanup removes its
        /// file and fails this test.
        #[test]
        fn retry_cleanup_removes_only_the_private_clone_stage() {
            let root = aterm_tempfile::Builder::new()
                .prefix("agh-clone-retry-cleanup-")
                .tempdir()
                .unwrap();
            let destination = root.path().join("destination");
            let stage = CloneStage::for_op(&GitOp::Mirror {
                dest: destination.clone(),
            })
            .unwrap()
            .unwrap();
            std::fs::write(stage.path().join("partial"), b"partial").unwrap();
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("caller's-file"), b"keep").unwrap();

            clear_clone_stage(Some(stage.path())).unwrap();

            assert!(std::fs::read_dir(stage.path()).unwrap().next().is_none());
            assert_eq!(
                std::fs::read(destination.join("caller's-file")).unwrap(),
                b"keep"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    /// A stall report names the credential-free operation and its exact internal phase, without
    /// copying ref rows or raw argv into the diagnostic.
    #[test]
    fn progress_wait_name_is_the_canonical_operation_and_step() {
        assert_eq!(
            super::unix::waiting_on(
                b"git fetch-batch fixture/app\nsource 0123\ninto refs/agh/x\nstep fetch"
            ),
            "git fetch-batch fixture/app, step fetch"
        );
        assert_eq!(
            super::unix::waiting_on(b"git ls-remote fixture/app\nref refs/heads/main"),
            "git ls-remote fixture/app"
        );
        assert_eq!(
            super::unix::waiting_on(b"not canonical"),
            "native git operation"
        );
    }

    #[test]
    fn cutter_helper_uses_only_its_explicit_agh_dispatch() {
        let helper = super::HelperCommand::for_executable("/owned/aterm-release".into()).unwrap();
        assert_eq!(helper.args, ["agh"]);
        assert_eq!(helper.exe, std::path::Path::new("/owned/aterm-release"));
        assert!(super::HelperCommand::for_executable("/owned/unrelated-cutter".into()).is_err());
    }

    #[test]
    fn main_is_fetched_into_no_branch_tag_or_remote_tracking_ref() {
        assert!(super::into_ok("refs/atpkg-publish/main-of-record"));
        for bad in [
            "refs/heads/main",
            "refs/tags/v1",
            "refs/remotes/origin/main",
            "refs/",
            "main",
            "refs/x/../y",
            "refs/x/a:b",
            "refs/x/.y",
        ] {
            assert!(!super::into_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_config_screen_names_every_rewriting_key_and_no_other() {
        use super::unix::rewrites;
        for key in [
            "url.https://x/.insteadof",
            "url.a.pushinsteadof",
            "http.proxy",
            "http.https://x/.sslcainfo",
            "remote.origin.proxy",
            "remote.origin.pushurl",
            "remote.origin.uploadpack",
            "remote.origin.receivepack",
            "remote.file:///x.url",
            "remote.http://127.0.0.1:1/a/b.git.fetch",
            "core.sshcommand",
            "core.gitProxy",
            "core.fsmonitor",
            "core.hookspath",
            "core.alternateRefsCommand",
            "credential.helper",
            "credential.https://github.com.username",
            "include.path",
            "includeif.gitdir:/x.path",
            "protocol.ext.allow",
            "extensions.partialClone",
            "fetch.bundleURI",
            "bundle.version",
            "bundle.one.uri",
            "remote.origin.promisor",
            "remote.origin.partialCloneFilter",
            "hook.x.command",
            "hook.x.event",
        ] {
            assert!(rewrites(key), "{key}");
        }
        for key in [
            "remote.origin.url",
            "remote.origin.fetch",
            "core.bare",
            "core.repositoryformatversion",
            "submodule.recurse",
            "submodule.sub.url",
            "branch.main.remote",
            "user.name",
            "url.insteadof",
            "fetch.prune",
            "extensions.objectformat",
            "extensions.worktreeconfig",
        ] {
            assert!(!rewrites(key), "{key}");
        }
    }

    #[test]
    fn http_transport_tuning_passes_the_config_screen() {
        // Independent of the implementation's allowlist: restoring the blanket HTTP refusal
        // fails every row. Mutating an allowed spelling must remain fail-closed.
        for var in [
            "postBuffer",
            "lowSpeedLimit",
            "lowSpeedTime",
            "maxRequests",
            "minSessions",
            "keepAliveIdle",
            "keepAliveInterval",
            "keepAliveCount",
        ] {
            for prefix in ["http.", "HTTP.https://github.com/owner/repo.git."] {
                let key = format!("{prefix}{var}");
                for read_only in [false, true] {
                    let project = |key: &str| {
                        super::unix::project_config_names(
                            format!("core.bare\0{key}\0").as_bytes(),
                            read_only,
                        )
                    };
                    let allowed: aterm_json::Value =
                        aterm_json::from_slice(&project(&key)).unwrap();
                    assert_eq!(
                        allowed,
                        aterm_json::json!({"Allowed": {"keys": 2}}),
                        "{key}"
                    );
                    let unknown = format!("{key}Future");
                    let refused: aterm_json::Value =
                        aterm_json::from_slice(&project(&unknown)).unwrap();
                    assert_eq!(
                        refused,
                        aterm_json::json!({"Refused": {"class": "Http"}}),
                        "{unknown}"
                    );
                }
            }
        }
    }

    #[test]
    fn http_security_protocol_and_unknown_options_stay_refused() {
        // Git 2.54 http_options minus the eight audited tuning variables; both scopes and
        // both read/write screens must refuse. Values never enter the screen's verdict.
        for var in [
            "proxy",
            "proxyAuthMethod",
            "proxySSLCert",
            "proxySSLKey",
            "proxySSLCertPasswordProtected",
            "proxySSLCAInfo",
            "emptyAuth",
            "proactiveAuth",
            "delegation",
            "extraHeader",
            "cookieFile",
            "saveCookies",
            "version",
            "curloptResolve",
            "sslVersion",
            "sslCipherList",
            "sslVerify",
            "sslCert",
            "sslKey",
            "sslCertPasswordProtected",
            "sslCAInfo",
            "sslCAPath",
            "sslBackend",
            "sslCertType",
            "sslKeyType",
            "schannelCheckRevoke",
            "schannelUseSSLCAInfo",
            "pinnedPubkey",
            "sslTry",
            "retryAfter",
            "maxRetries",
            "maxRetryTime",
            "noEPSV",
            "userAgent",
            "followRedirects",
            "futureOption",
            "",
            "postBuffer.extraHeader",
        ] {
            for prefix in ["http.", "HtTp.https://github.com/owner/repo.git."] {
                let key = format!("{prefix}{var}\0");
                for read_only in [false, true] {
                    let verdict: aterm_json::Value = aterm_json::from_slice(
                        &super::unix::project_config_names(key.as_bytes(), read_only),
                    )
                    .unwrap();
                    assert_eq!(
                        verdict,
                        aterm_json::json!({"Refused": {"class": "Http"}}),
                        "{key}"
                    );
                }
            }
        }
    }

    /// Step 9 on a leased delete's own push: an exit no phrase names, or a signal, may follow a
    /// delete GitHub applied, so it is could_not_run with the side effect unknown; the same
    /// exit of a read, or of the push's scratch `init` (`sent` false), wrote nothing. A
    /// porcelain `[remote rejected]` line is Refused, nothing written.
    #[test]
    fn a_push_s_unnamed_exit_may_have_been_sent() {
        use crate::error::{Kind, SideEffect};
        use crate::fx::child::ChildOut;
        let redactor = crate::redact::Redactor::new();
        let slug = crate::slug::Slug::new("fixture-dev", "app").expect("a slug");
        let push = super::GitOp::PushDelete {
            reference: "refs/tags/v1".to_string(),
            expect: "0123456789abcdef0123456789abcdef01234567".to_string(),
        };
        let read = super::GitOp::LsRemote {
            reference: None,
            anonymous: false,
        };
        let hung = ChildOut {
            code: Some(128),
            stdout: Vec::new(),
            stderr: b"fatal: the remote end hung up unexpectedly\n".to_vec(),
        };
        let signalled = ChildOut {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        for out in [&hung, &signalled] {
            let error = super::unix::failed(&push, out, &redactor, &slug, true);
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::CouldNotRun, SideEffect::Unknown),
                "{}",
                error.why
            );
            for (op, sent) in [(&read, true), (&push, false)] {
                let error = super::unix::failed(op, out, &redactor, &slug, sent);
                assert_eq!(
                    (error.kind, error.side_effect),
                    (Kind::CouldNotRun, SideEffect::None),
                    "{op:?}, sent {sent}: {}",
                    error.why
                );
            }
        }
        let ruled = ChildOut {
            code: Some(1),
            stdout: b"To http://127.0.0.1:1/fixture-dev/app.git\n!\t:refs/tags/v1\t[remote rejected] (protected)\nDone\n"
                .to_vec(),
            stderr: b"error: failed to push some refs to 'http://127.0.0.1:1/fixture-dev/app.git'\n"
                .to_vec(),
        };
        let error = super::unix::failed(&push, &ruled, &redactor, &slug, true);
        assert_eq!(
            (error.kind, error.side_effect, error.exit()),
            (Kind::Refused, SideEffect::None, 1),
            "{}",
            error.why
        );
        assert!(error.why.contains("refs/tags/v1"), "{}", error.why);
        for (reason, transient) in [
            ("failed", true),
            ("failed to update ref", true),
            ("failed to update refs", true),
            ("transaction failed to start", true),
            ("atomic transaction failed", true),
            ("unable to migrate objects to permanent storage", true),
            ("protected branch hook declined", false),
            ("pre-receive hook declined", false),
            ("hook declined", false),
            ("protected", false),
            ("permission denied", false),
            ("repository rule violations found", false),
            ("unrecognized failure", false),
            ("atomic push failure", false),
        ] {
            let mut out = ruled.clone();
            out.stdout = format!("!\t:refs/tags/v1\t[remote rejected] ({reason})\n").into_bytes();
            let error = super::unix::failed(&push, &out, &redactor, &slug, true);
            assert_eq!(
                (error.kind, error.side_effect),
                if transient {
                    (Kind::CouldNotRun, SideEffect::Unknown)
                } else {
                    (Kind::Refused, SideEffect::None)
                },
                "{reason}"
            );
            assert!(error.why.contains(reason));
        }
        for (other, expected) in [
            ("atomic push failure", Kind::CouldNotRun),
            ("protected branch hook declined", Kind::Refused),
        ] {
            let mut out = ruled.clone();
            out.stdout =
                format!("!\t:a\t[remote rejected] ({other})\n!\t:b\t[remote rejected] (failed)\n")
                    .into_bytes();
            assert_eq!(
                super::unix::failed(&push, &out, &redactor, &slug, true).kind,
                expected
            );
            // Reordering rows must preserve a concrete policy refusal.
            out.stdout =
                format!("!\t:b\t[remote rejected] (failed)\n!\t:a\t[remote rejected] ({other})\n")
                    .into_bytes();
            assert_eq!(
                super::unix::failed(&push, &out, &redactor, &slug, true).kind,
                expected
            );
        }
        let stale = ChildOut {
            code: Some(1),
            stdout: b"!\t(delete):refs/tags/v1\t[rejected] (stale info)\n".to_vec(),
            stderr: Vec::new(),
        };
        let error = super::unix::failed(&push, &stale, &redactor, &slug, true);
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::Conflict, SideEffect::None)
        );
    }

    /// A learned token that straddles the 64 KiB cut is redacted whole before the cut, so no
    /// tail of it is kept.
    #[test]
    fn a_token_across_the_cut_leaves_no_tail() {
        let redactor = crate::redact::Redactor::new();
        let token: String = ["kept", "-learned-", "value", "-TAILQZX"].concat();
        redactor.learn(token.as_bytes());
        let pad = "y".repeat(64 * 1024 - 4);
        let bytes = format!("{token}{pad}");
        let kept = super::unix::kept(bytes.as_bytes(), &redactor);
        assert!(!kept.contains("QZX"), "a tail of the token was kept");
        assert!(kept.len() <= 64 * 1024, "{}", kept.len());
        let tail = format!("{}\u{e9}", "z".repeat(64 * 1024));
        let kept = super::unix::kept(tail.as_bytes(), &redactor);
        assert!(kept.ends_with('\u{e9}'), "the cut kept the last char whole");
    }

    /// NEGATIVE CONTROL: transient git scratch directories must carry `.noindex` so Spotlight
    /// never indexes cloned worktrees or credential-helper homes under `$TMPDIR`.
    #[test]
    fn git_run_scratch_directory_carries_noindex_suffix() {
        let (dir, root) = super::unix::scratch().expect("create git scratch");
        let name = dir
            .path()
            .file_name()
            .and_then(|os| os.to_str())
            .expect("scratch dir name is UTF-8");
        assert!(name.starts_with("agh-git-"), "{name}");
        assert!(name.ends_with(".noindex"), "{name}");
        assert!(root.ends_with(name), "{}", root.display());
        assert!(root.join("home").is_dir());
        assert!(root.join("work").is_dir());
    }
}
