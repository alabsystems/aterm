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

/// What the runner runs. There is no free-form argv.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitOp {
    /// Admit local object reads under the same source-config and discovery guards.
    /// No remote request, credentials, grant or source update participates.
    CheckLocal {
        /// Existing source checkout or bare repository.
        dir: PathBuf,
    },
    /// `git clone --quiet [--depth N] -- <url> <dest>`, `<dest>` absent or empty.
    Clone {
        /// Where to.
        dest: PathBuf,
        /// `--depth`.
        depth: Option<u32>,
    },
    /// `git clone --quiet --mirror -- <url> <dest>`: a bare snapshot of all refs.
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
    /// `git ls-remote --refs <url> [<ref>]`.
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
    /// The run's bound (`GIT_BUDGET` by default).
    pub budget: Duration,
}

impl GitRun {
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
    /// `op` on `slug`, `Named::Default`, bounded by `GIT_BUDGET`.
    fn of(slug: &Slug, op: GitOp) -> GitRun {
        GitRun {
            slug: slug.clone(),
            op,
            named: Named::Default,
            budget: GIT_BUDGET,
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

    /// The same run with another bound.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> GitRun {
        self.budget = budget;
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

/// What a run printed: stdout and stderr, each redacted and cut to its last 64 KiB, and who it
/// spoke as.
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
mod unix {
    //! The runner's steps, on unix.

    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use super::{GitOp, GitOut, GitRun};
    use crate::client::Github;
    use crate::config::Dirs;
    use crate::error::{Error, Forbid, Kind, Result, SideEffect, Sub};
    use crate::fx::child::{ChildOut, ChildSpec};
    use crate::git::GRANT_MAX;
    use crate::git::credential;
    use crate::git::grant::{self, Grant};
    use crate::identity::{EnvSource, Source};
    use crate::policy::{Decision, Named, RefusalCode, RouteRequest, RouteTarget, Tier};
    use crate::redact::Redactor;
    use crate::slug::Slug;

    /// The route door's fact naming where a profile's token lives (`Fx::fact`).
    const SOURCE: &str = "source";

    /// The canonical request of a leased delete's scratch repository, `git init --quiet
    /// --bare`: a fixed one, so a taped leased delete replays with nothing run.
    const INIT_SCRATCH: &str = "git init-scratch";
    /// The canonical request of a fetch's config read, followed by ` {owner}/{repo}`.
    const CONFIG_LIST: &str = "git config-list";
    /// How much of each stream a `GitOut` keeps: its last 64 KiB.
    const OUT_KEEP: usize = 64 * 1024;
    /// The longest config listing step 3 screens: 256 KiB of key names, far past any real
    /// clone's and well under the 1 MiB tail the child door keeps of a stream, so a listing
    /// the door cut is always longer than this and refused.
    const SCREEN_MAX: usize = 256 * 1024;
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
                    Redactor::shapes(into)
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
        /// The run's bound.
        budget: std::time::Duration,
        /// Whether the run only reads, so its main child killed at its bound changed nothing.
        /// The children the runner makes for itself (a fetch's config read, a leased delete's
        /// scratch `init`) send nothing whatever this says: `unsent` lowers their errors.
        reads: bool,
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
            self.git_supplied(args, cwd, canonical, None)
        }

        fn git_input(
            &self,
            args: &[OsString],
            cwd: Option<&Path>,
            canonical: &[u8],
            input: &[u8],
        ) -> Result<ChildOut> {
            self.git_supplied(args, cwd, canonical, Some(input))
        }

        fn git_supplied(
            &self,
            args: &[OsString],
            cwd: Option<&Path>,
            canonical: &[u8],
            input: Option<&[u8]>,
        ) -> Result<ChildOut> {
            let mut spec = ChildSpec::new(&self.git).budget(self.budget);
            if let Some(input) = input {
                spec = spec.input(input);
            }
            for arg in args {
                spec = spec.arg(arg.clone());
            }
            for (name, value) in &self.env {
                spec.env.push((name.clone(), value.clone()));
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
        | GitOp::Fetch { dir }
        | GitOp::FetchMain { dir, .. }
        | GitOp::FetchRef { dir, .. }
        | GitOp::FetchBatch { dir, .. }
        | GitOp::PushRefs { dir, .. }) = op
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
            GitOp::Clone { .. } | GitOp::Mirror { .. } => "clone",
            GitOp::Fetch { .. }
            | GitOp::FetchMain { .. }
            | GitOp::FetchRef { .. }
            | GitOp::FetchBatch { .. } => "fetch",
            GitOp::LsRemote { .. } => "ls-remote",
            GitOp::PushDelete { .. } | GitOp::PushRefs { .. } => "push",
        }
    }

    /// `bytes` as text, redacted whole and then cut to about its last `OUT_KEEP` bytes, at a
    /// char boundary. Redacting first means a token that straddles the cut is replaced whole,
    /// never left as a tail that neither the learned value nor a token shape matches.
    pub(super) fn kept(bytes: &[u8], redactor: &Redactor) -> String {
        let text = redactor.apply(&String::from_utf8_lossy(bytes));
        let from = text.len().saturating_sub(OUT_KEEP);
        let from = (from..=text.len())
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(text.len());
        text.get(from..).unwrap_or_default().to_string()
    }

    /// Step 3: whether a clone's local config key can send git elsewhere, through a proxy, or
    /// into a program of the clone's choosing. That includes a bundle URI a fetch reads first
    /// (`fetch.bundleuri`, `bundle.*`) and a partial clone's promisor, which git fetches from
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
        } else if is(
            section,
            &[
                "http",
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
            sub.is_none() && is(var, &["worktreeconfig", "partialclone"])
        } else {
            false
        }
    }

    /// Step 3's screen over `git config --local --list --name-only -z`'s output: a listing
    /// longer than `SCREEN_MAX`, or the first key that rewrites where git goes, Refused. The
    /// child door keeps only the tail of a stream (`fx::child`, the last 1 MiB), so a listing
    /// past `SCREEN_MAX` may have lost its head, and a key cut off there would pass the screen
    /// while git still reads it: the screen fails closed on the length alone. Listing names
    /// only keeps a value from padding the listing; a long subsection name still can.
    pub(super) fn screen(listed: &[u8], slug: &Slug, read_only: bool) -> Result<()> {
        if listed.len() > SCREEN_MAX {
            return Err(Error::refused(format!(
                "the clone's config is too long to screen ({} bytes of names)",
                listed.len()
            ))
            .with_next("remove the padding from the clone's .git/config, or clone again")
            .with_repo(slug.to_string()));
        }
        for entry in listed.split(|b| *b == 0).filter(|e| !e.is_empty()) {
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
                let key = Redactor::shapes(&key);
                return Err(Error::refused(format!(
                    "the clone's config rewrites where git goes: {key}"
                ))
                .with_sub(Sub::GitConfig { key })
                .with_next("remove the key from the clone's .git/config, or clone again")
                .with_repo(slug.to_string()));
            }
        }
        Ok(())
    }

    /// Step 9: an exit that is not 0, mapped. Each phrase is looked for on stderr and stdout
    /// both, because `push --porcelain` reports a refused lease on stdout. `sent` is true only
    /// for the exit of the leased delete's own `git push`: an exit no phrase names there (a
    /// hung-up remote, an HTTP error, a signal agh did not send) may follow a delete GitHub
    /// applied, so it is could_not_run with `SideEffect::Unknown`, never read as not sent. A
    /// porcelain line `!` with `[rejected]` or `[remote rejected]` (GitHub's protected branch
    /// or ruleset) is a refusal no retry changes: Refused, nothing written.
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
            Error::refused(format!("GitHub refused the push: {line}"))
        } else {
            let code = out
                .code
                .map_or_else(|| "on a signal".to_string(), |code| code.to_string());
            let error = Error::could_not_run(format!("git {} exited {code}: {last}", op_word(op)))
                .with_sub(Sub::Git { exit: out.code });
            if sent
                && matches!(
                    op,
                    GitOp::PushDelete { .. } | GitOp::PushRefs { dry_run: false, .. }
                )
            {
                error.with_side_effect(SideEffect::Unknown)
            } else {
                error
            }
        };
        error.with_repo(slug.to_string())
    }

    /// A leased delete's porcelain line (stdout, already redacted) that GitHub or git refused
    /// for good: it starts `!` and names `[rejected]` or `[remote rejected]`, and is not the
    /// lease's `stale info`, which is Conflict.
    fn rejected(op: &GitOp, stdout: &str) -> Option<String> {
        if !matches!(op, GitOp::PushDelete { .. } | GitOp::PushRefs { .. }) {
            return None;
        }
        stdout
            .lines()
            .map(str::trim)
            .find(|line| {
                line.starts_with('!')
                    && (line.contains("[rejected]") || line.contains("[remote rejected]"))
                    && !line.contains("stale info")
            })
            .map(str::to_string)
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
        let push = matches!(run.op, GitOp::PushDelete { .. } | GitOp::PushRefs { .. });
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
                reads: !push,
            };
            return finish(&children, run, &remote, profile, None);
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
        let children = Children {
            gh,
            git,
            env,
            work: Some(root.join("work")),
            scratch_git: (push
                || matches!(run.op, GitOp::FetchRef { .. } | GitOp::FetchBatch { .. }))
            .then(|| root.join("scratch.git")),
            budget: run.budget,
            reads: !push,
        };
        let nonce = guard.as_ref().map(|g| g.grant.nonce.clone());
        let result = finish(&children, run, &remote, profile, nonce);
        drop(guard);
        drop(scratch);
        result
    }

    /// Step 3 of a fetch: `dir`, absolute, once `git config --local --list` of it passed the
    /// config screen (`screen`).
    fn screened(children: &Children<'_>, run: &GitRun, dir: &Path) -> Result<PathBuf> {
        let slug = &run.slug;
        let dir = std::path::absolute(dir)
            .map_err(|e| Error::local(format!("{}: {e}", dir.display())))?;
        let listed = children
            .git(
                &[
                    os("-C"),
                    dir.clone().into_os_string(),
                    os("config"),
                    os("--local"),
                    os("--list"),
                    os("--name-only"),
                    os("-z"),
                ],
                None,
                format!("{CONFIG_LIST} {slug}").as_bytes(),
            )
            .map_err(unsent)?;
        if listed.code != Some(0) {
            return Err(failed(
                &run.op,
                &listed,
                children.gh.redactor(),
                slug,
                false,
            ));
        }
        let read_only = matches!(
            &run.op,
            GitOp::CheckLocal { .. }
                | GitOp::Fetch { .. }
                | GitOp::FetchMain { .. }
                | GitOp::FetchRef { .. }
                | GitOp::FetchBatch { .. }
        );
        screen(&listed.stdout, slug, read_only)?;
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
            let out = children.git(&args, cwd, &key).map_err(unsent)?;
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
        let common = step(
            "common-dir",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("rev-parse"),
                os("--path-format=absolute"),
                os("--git-common-dir"),
            ],
            None,
        )?;
        let common = std::str::from_utf8(&common.stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|text| !text.contains(['\n', '\r']) && Path::new(text).is_absolute())
            .ok_or_else(|| Error::local("git returned an invalid common directory"))?;
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
            let out = match input {
                Some(input) => children.git_input(&args, cwd, &key, input),
                None => children.git(&args, cwd, &key),
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
        let common = step(
            "common-dir",
            vec![
                os("-C"),
                dir.as_os_str().to_os_string(),
                os("rev-parse"),
                os("--path-format=absolute"),
                os("--git-common-dir"),
            ],
            None,
            None,
        )?;
        let common = std::str::from_utf8(&common.stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|text| !text.contains(['\n', '\r']) && Path::new(text).is_absolute())
            .ok_or_else(|| Error::local("git returned an invalid common directory"))?;
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
        use std::collections::BTreeMap;
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
        let mut import = vec![
            os("-c"),
            os("protocol.file.allow=always"),
            os("fetch"),
            os("--quiet"),
            os("--no-tags"),
            os("--no-write-fetch-head"),
            os("--recurse-submodules=no"),
            dir.as_os_str().to_owned(),
        ];
        for (i, u) in batch.updates().iter().enumerate() {
            import.push(os(format!("{}:refs/agh/source/{i}", u.after)));
        }
        let incremental = batch.updates().iter().any(|u| u.before != ZERO);
        if !incremental {
            checked("objects", import, scratch)?;
        }
        let mut list = vec![os("ls-remote"), os("--refs"), os(remote.url())];
        list.extend(batch.updates().iter().map(|u| os(&u.reference)));
        let listed = checked("remote", list, scratch)?;
        let text = std::str::from_utf8(&listed.stdout)
            .map_err(|_| Error::protocol("git returned non-UTF-8 refs"))?;
        let mut found = BTreeMap::new();
        for line in text.lines() {
            let (oid, reference) = line
                .split_once('\t')
                .ok_or_else(|| Error::protocol("git returned malformed refs"))?;
            if !crate::path::is_sha(oid)
                || !batch.updates().iter().any(|u| u.reference == reference)
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
        // Only a freshly observed remote BEFORE can exclude an old object
        // closure. Source refs, grafts, replacement objects and accelerator
        // files are never copied into this private read/write repository.
        let alternate = scratch.map(|p| p.join("objects/info/alternates"));
        if incremental {
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
            let mut metadata_roots = String::new();
            for u in batch.updates() {
                metadata_roots.push_str(&u.after);
                metadata_roots.push('\n');
                if u.before != ZERO {
                    metadata_roots.push_str(&u.before);
                    metadata_roots.push('\n');
                }
            }
            let prefix = scratch
                .map_or_else(|| PathBuf::from("scratch.git"), Path::to_path_buf)
                .join("objects/pack/pack");
            let metadata = checked_input(
                "metadata-pack",
                vec![
                    os("-c"),
                    os("core.commitGraph=false"),
                    os("-c"),
                    os("pack.useBitmaps=false"),
                    os("pack-objects"),
                    os("--quiet"),
                    os("--revs"),
                    os("--filter=blob:none"),
                    os("--threads=8"),
                    os("--no-use-bitmap-index"),
                    prefix.as_os_str().to_owned(),
                ],
                metadata_roots.as_bytes(),
            )?;
            let hash = std::str::from_utf8(&metadata.stdout)
                .ok()
                .and_then(|s| s.strip_suffix('\n'))
                .filter(|s| crate::path::is_sha(s))
                .ok_or_else(|| Error::protocol("metadata pack ID is malformed"))?;
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
            )?;
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
            args.extend(batch.updates().iter().map(|u| os(&u.after)));
            // Connectivity-only does not hash every tree. The full metadata
            // pack above independently verifies all commit/tree/tag bytes and
            // their original index IDs before any old closure is excluded.
            // This extra walk checks referenced blob presence. New blob bytes
            // are checked below; old blob bytes are already held by BEFORE.
            checked("connectivity", args, scratch)?;
        }
        for (i, u) in batch.updates().iter().enumerate() {
            let kind = checked(
                &format!("kind-{i}"),
                vec![os("cat-file"), os("-t"), os(&u.after)],
                scratch,
            )?;
            if u.reference.starts_with("refs/heads/") && kind.stdout != b"commit\n" {
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
            if kind.stdout != b"commit\n" {
                return Err(Error::refused(
                    "an existing ref can advance only to a descendant commit",
                ));
            }
            let before_kind = checked(
                &format!("before-kind-{i}"),
                vec![os("cat-file"), os("-t"), os(&u.before)],
                scratch,
            )?;
            if before_kind.stdout != b"commit\n" {
                return Err(Error::refused("an existing non-commit ref is immutable"));
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
                Some(0) => {}
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
        if incremental {
            let mut roots = String::new();
            for u in batch.updates() {
                roots.push_str(&u.after);
                roots.push('\n');
            }
            for u in batch.updates().iter().filter(|u| u.before != ZERO) {
                roots.push('^');
                roots.push_str(&u.before);
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
        final_list.extend(batch.updates().iter().map(|u| os(&u.reference)));
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
        args.extend(
            batch
                .updates()
                .iter()
                .map(|u| os(format!("{}:{}", u.after, u.reference))),
        );
        let out = children
            .git(&args, scratch, &canonical)
            .map_err(|e| if dry_run { unsent(e) } else { e })?;
        if out.code != Some(0) {
            return Err(failed(
                &run.op,
                &out,
                children.gh.redactor(),
                &run.slug,
                !dry_run,
            ));
        }
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
            if !matches!(flag, " " | "*")
                || !batch
                    .updates()
                    .iter()
                    .any(|u| u.reference == reference && u.after == source)
                || !acknowledged.insert(reference)
            {
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
            GitOp::Mirror { dest } => {
                let dest = std::path::absolute(dest)
                    .map_err(|e| Error::local(format!("{}: {e}", dest.display())))?;
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
                let dest = std::path::absolute(dest)
                    .map_err(|e| Error::local(format!("{}: {e}", dest.display())))?;
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
                if !anonymous {
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
        let out = children.git(&args, cwd, &canonical)?;
        if out.code != Some(0) {
            return Err(failed(&run.op, &out, redactor, slug, true));
        }
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
}

#[cfg(all(test, unix))]
mod tests {
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
            "extensions.worktreeConfig",
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
        ] {
            assert!(!rewrites(key), "{key}");
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
