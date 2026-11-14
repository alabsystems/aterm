// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Git queries on a checkout through [`aterm_uds::spawnfd::command`].

use std::ffi::OsString;
use std::path::Path;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aterm_uds::deadline::{CompleteOutputError, MAX_COMPLETE_STDOUT_BYTES, RunError};

use crate::error::{Error, Result};

/// A caller-owned, frozen repository admitted by Agh's native local-read guard.
/// No credentials or network route is loaded. Admission does not freeze the caller's
/// directory against concurrent replacement; publication owns that source lifetime.
pub struct SourceRepo {
    root: std::path::PathBuf,
}

impl SourceRepo {
    /// Admit a local repository without changing its configuration, refs or objects.
    /// # Errors
    /// Native source/discovery/config/physical-path admission refusal.
    pub fn open(root: &Path) -> Result<Self> {
        let root =
            std::fs::canonicalize(root).map_err(|e| Error::io("resolve source repository", &e))?;
        let gh = agh::GithubBuilder::new(agh::policy::Router::empty())
            .fx(std::sync::Arc::new(agh::Fx::live()))
            .build();
        let slug = agh::Slug::new("alab", "local").map_err(|e| Error::refused(e.to_string()))?;
        gh.run_git(&agh::git::GitRun::check_local(&slug, &root))
            .map_err(|e| Error::refused(format!("source read admission: {e}")))?;
        Ok(Self { root })
    }

    /// The canonical repository admitted for these object reads.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Run a bounded local metadata operation with no ambient Git routing, replacement or
    /// lazy fetch. Callers use object queries and remove only their own temporary refs.
    /// # Errors
    /// Missing objects, invalid output, spawn or bounded collection failure.
    pub(crate) fn query(&self, args: &[&str]) -> Result<String> {
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            cmd.env_remove(key);
        }
        cmd.args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(&self.root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
        let out = output_within(&mut cmd, LOCAL_GIT_BOUND)
            .map_err(|e| Error::cannot_run(format!("read immutable source object: {e}")))?;
        if !out.status.success() {
            return Err(Error::refused("immutable source objects could not be read"));
        }
        String::from_utf8(out.stdout).map_err(|_| Error::refused("source metadata is not UTF-8"))
    }
}

static MAIN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Remove ambient credentials, injected command-scope config, TLS overrides and all inherited
/// trace selectors before a credential helper runs: what a network git's environment drops,
/// whichever helper it mounts.
pub(crate) fn git_environment_removals() -> Vec<String> {
    git_environment_removals_from(std::env::vars_os().map(|(key, _)| key))
}

/// The unscoped `-c` pairs a network git that hands over a credential carries, as agh's own git
/// runner pins them: TLS verification on and no HTTP redirect, so a credential answered for one
/// URL is never sent on to another. They override an unscoped key (`http.sslVerify=false`) in
/// any config scope, since git reads the command line last; `GIT_SSL_NO_VERIFY`, which git
/// reads over any config, is one of [`git_environment_removals`]. They do not override a key
/// scoped to a URL the remote matches (`http."https://github.com".sslVerify=false`), which git
/// ranks above an unscoped one whatever its scope: a git given a URL also carries
/// [`git_tls_pins_at`] that URL. agh's runner needs no scoped pair, since it reads neither the
/// user's nor the system's config and refuses a clone whose own config holds an `http.*` key
/// other than transport tuning (`http.postBuffer`, the low-speed and keep-alive limits).
pub(crate) const GIT_TLS_PINS: [&str; 4] = [
    "-c",
    "http.sslVerify=true",
    "-c",
    "http.followRedirects=false",
];

/// [`GIT_TLS_PINS`]' keys scoped to exactly `url`, the URL a network git is given: git resolves
/// `http.<url>.*` by URL match, and `url` itself is the most specific key its remote can match
/// (a key with a longer path is no prefix of it, one with a user name matches only a URL that
/// names that user, and one with a wildcard host is no longer than the exact host), so a key at
/// an equally specific spelling of it, in the user's, the system's or the checkout's config or
/// an inherited command-scope variable, ties, and git takes the command line's value, read last.
/// Nothing for a URL git does not fetch over HTTP (an SSH or `file://` remote). This pins the URL git is given: a
/// `url.<base>.insteadOf` rewrite of it to another HTTP URL reaches that URL with the unscoped
/// pins alone, so a caller that cannot rule a rewrite out (the harness fetch does, comparing
/// `git remote get-url`, which expands every rewrite, with its development repository) says so.
/// # Errors
/// A `url` holding `=`, which would end a `-c` key early.
pub(crate) fn git_tls_pins_at(url: &str) -> std::result::Result<Vec<String>, String> {
    let http = url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
    });
    if !http {
        return Ok(Vec::new());
    }
    if url.contains('=') {
        return Err(format!(
            "{url} cannot be pinned as a git config key: it holds '='"
        ));
    }
    Ok(vec![
        "-c".into(),
        format!("http.{url}.sslVerify=true"),
        "-c".into(),
        format!("http.{url}.followRedirects=false"),
    ])
}

/// Environment overrides also disable Trace2 targets enabled by system/global Git config.
pub(crate) fn git_tracing_environment() -> Vec<(String, OsString)> {
    [
        ("GIT_TRACE", "0"),
        ("GIT_TRACE2", "0"),
        ("GIT_TRACE2_EVENT", "0"),
        ("GIT_TRACE2_PERF", "0"),
        ("GIT_TRACE_CURL", "0"),
        ("GIT_TRACE_REDACT", "1"),
    ]
    .map(|(name, value)| (name.into(), value.into()))
    .into()
}

/// The removals for an environment holding `names`: the fixed ambient-token, injected-config,
/// verbose and shell start-up selectors, every `GIT_TRACE*`, `GIT_SSL_*`, `GIT_CONFIG_KEY_*`,
/// `GIT_CONFIG_VALUE_*`, `GH_*`, `GITHUB_*` and `AGH_GIT_*` name present, and
/// `ATPKG_PUBLISH_GIT_TOKEN`, the variable the token host's helper reads its token from.
/// `GIT_CONFIG_PARAMETERS`, and `GIT_CONFIG_COUNT` with its `GIT_CONFIG_KEY_<n>` and
/// `GIT_CONFIG_VALUE_<n>` pairs, are config git reads in command scope, as it reads `-c`, so an
/// inherited one could carry a URL-scoped `http.*` key or a `url.*.insteadOf` rewrite past a
/// git's own words; agh's own runner never passes them. A `GIT_SSL_*` name overrides git's TLS
/// configuration from the environment (`GIT_SSL_NO_VERIFY` turns verification off over any
/// config, `GIT_SSL_CAINFO` and `GIT_SSL_CAPATH` choose whom it trusts), which agh's own runner
/// never passes. An `AGH_GIT_*` name is what agh's git runner hands its own credential helper (a
/// grant, the loose policy, its directories), so agh's helper mounted on a git without the
/// runner routes from this process's own agh directories, with no grant, and by agh.toml's
/// strict policy unless the caller sets the loose one itself (`agh::git::LOOSE_VAR`).
pub(crate) fn git_environment_removals_from(
    names: impl IntoIterator<Item = OsString>,
) -> Vec<String> {
    let mut remove = std::collections::BTreeSet::from([
        "ATPKG_PUBLISH_GIT_TOKEN".to_owned(),
        "GITHUB_TOKEN".into(),
        "GH_TOKEN".into(),
        "GIT_CONFIG_COUNT".into(),
        "GIT_CONFIG_PARAMETERS".into(),
        "GIT_CURL_VERBOSE".into(),
        "BASH_ENV".into(),
        "ENV".into(),
        "SHELLOPTS".into(),
        "BASHOPTS".into(),
    ]);
    for name in names {
        if let Some(name) = name.to_str()
            && (name.starts_with("GIT_TRACE")
                || name.starts_with("GIT_SSL_")
                || name.starts_with("GIT_CONFIG_KEY_")
                || name.starts_with("GIT_CONFIG_VALUE_")
                || name.starts_with("GH_")
                || name.starts_with("GITHUB_")
                || name.starts_with("AGH_GIT_"))
        {
            remove.insert(name.to_owned());
        }
    }
    remove.into_iter().collect()
}

/// Admit local source reads and require that its origin names the requested repository.
/// This checks local configuration only; fetching and authenticating the remote remain
/// the selected host's responsibility.
/// A refusal names the origin it found, its userinfo masked ([`without_userinfo`]). Only a
/// checkout whose remotes (`git remote`) include no `origin` is told it has none; an origin
/// that is there and cannot be read (its URL not UTF-8, a failing query) keeps the query's own
/// refusal.
/// # Errors
/// Local source admission, a missing or unreadable origin or an origin for another repository.
pub(crate) fn source_with_origin(repo: &Path, slug: &crate::host::Slug) -> Result<SourceRepo> {
    let source = SourceRepo::open(repo)?;
    if !source
        .query(&["remote"])?
        .lines()
        .any(|name| name == "origin")
    {
        return Err(Error::refused(format!(
            "the source has no origin remote; it must name {slug}"
        )));
    }
    let origin = source.query(&["remote", "get-url", "origin"])?;
    let origin = origin.trim();
    if !crate::artifact::package::names_repo(origin, &slug.owner, &slug.repo) {
        return Err(Error::refused(format!(
            "the source origin does not name {slug} (it names {}); its main cannot judge this cut",
            without_userinfo(origin)
        )));
    }
    Ok(source)
}

/// `url` with the userinfo of a `scheme://userinfo@host/…` URL (a user name, a password, a
/// token) replaced by `***`, so a message can name a remote without its credential. Any other
/// form is returned whole: an scp-style `user@host:path` carries no password.
fn without_userinfo(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let authority = rest.find('/').map_or(rest, |end| &rest[..end]);
        if let Some(at) = authority.rfind('@') {
            return format!("{scheme}://***@{}", &rest[at + 1..]);
        }
    }
    url.to_string()
}

/// Read a repository's owner-bound main through the selected host, preserving the checkout,
/// origin/main and FETCH_HEAD. The native route never falls back to the publisher's raw
/// network Git or ambient origin credentials.
pub(crate) fn main_of_record(
    host: &dyn crate::host::ReleaseHost,
    repo: &Path,
    slug: &crate::host::Slug,
) -> Result<(SourceRepo, String)> {
    let source = source_with_origin(repo, slug)?;
    let reference = format!(
        "refs/agh/atpkg-publish/main-{}-{}",
        std::process::id(),
        MAIN_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let fetched = host
        .fetch_main(slug, source.root(), &reference)
        .map_err(|error| Error::cannot_run(format!("read {slug}'s main from its host: {error}")))
        .and_then(|()| source.query(&["rev-parse", "--verify", &reference]));
    let dropped = source.query(&["update-ref", "-d", &reference]);
    let tip = fetched?;
    dropped?;
    let tip = tip.trim();
    if tip.len() != 40
        || !tip
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::refused(
            "the host's main did not name a full raw commit OID",
        ));
    }
    if source.query(&["cat-file", "-t", tip])?.trim() != "commit" {
        return Err(Error::refused(
            "the host's main named another Git object kind",
        ));
    }
    Ok((source, tip.to_owned()))
}

/// How long a local git query or checkout ([`run`]) may take before it is taken for hung and
/// killed: ten minutes, where a checkout of a compiler's tree on a loaded machine takes one or two.
pub const LOCAL_GIT_BOUND: Duration = Duration::from_mins(10);

/// Runs a git `cmd` to its end within `within`, its stdout whole: more than
/// [`MAX_COMPLETE_STDOUT_BYTES`] is refused, never a kept tail read as the whole (a first-parent
/// `rev-list` prints its newest commits first, which a tail drops), and a git that has not ended
/// by then is killed with its process group.
/// # Errors
/// A spawn refusal ([`RunError::Spawn`]), the deadline, or a stdout that is not whole.
pub(crate) fn output_within(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: Duration,
) -> std::result::Result<Output, CompleteOutputError> {
    aterm_uds::deadline::output_until_complete_stdout_with_limit(
        cmd,
        Instant::now() + within,
        MAX_COMPLETE_STDOUT_BYTES,
    )
}

/// `git -C <dir> <args…>`'s trimmed stdout, within [`LOCAL_GIT_BOUND`] ([`output_within`]).
/// # Errors
/// A could-not-run when git cannot be started, does not end in time or prints more than a whole
/// stdout can hold, or a refusal when git fails.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    let out = output_within(&mut cmd, LOCAL_GIT_BOUND).map_err(|e| match e {
        CompleteOutputError::Run(RunError::Spawn(e)) => Error::io("spawn git", &e),
        other => Error::cannot_run(format!(
            "`git -C {} {}`: {other}",
            dir.display(),
            args.join(" ")
        )),
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(Error::refused(format!(
            "`git -C {} {}` failed: {}",
            dir.display(),
            args.join(" "),
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        )))
    }
}

/// Whether a file of `rev` in `dir` that matches `pathspecs` holds `word`, read from the
/// commit's own objects (`git grep <rev>`): no work tree is needed or touched.
/// # Errors
/// A could-not-run when git cannot be started or does not end in time; a refusal when git
/// fails for any reason but "no match".
pub fn tree_holds(dir: &Path, rev: &str, word: &str, pathspecs: &[&str]) -> Result<bool> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    cmd.arg("-C")
        .arg(dir)
        .args(["grep", "--quiet", "--fixed-strings", "-e", word, rev, "--"])
        .args(pathspecs)
        .env("GIT_TERMINAL_PROMPT", "0");
    let out = output_within(&mut cmd, LOCAL_GIT_BOUND).map_err(|e| match e {
        CompleteOutputError::Run(RunError::Spawn(e)) => Error::io("spawn git", &e),
        other => Error::cannot_run(format!("`git -C {} grep {rev}`: {other}", dir.display())),
    })?;
    match out.status.code() {
        Some(0) => Ok(true),
        // `git grep` exits 1 for "nothing matched", and says nothing.
        Some(1) if out.stderr.is_empty() => Ok(false),
        _ => Err(Error::refused(format!(
            "`git -C {} grep {rev}` failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        ))),
    }
}

/// The build number of `rev` in `dir`.
/// # Errors
/// When the repository is shallow, git fails, or its output is not recognized.
pub fn rev_count(dir: &Path, rev: &str) -> Result<u64> {
    match run(dir, &["rev-parse", "--is-shallow-repository"])?.as_str() {
        "false" => {}
        "true" => {
            return Err(Error::refused(format!(
                "{} is shallow; publication build numbers require complete history. \
                 Use a full clone or fetch --unshallow before publishing",
                dir.display()
            )));
        }
        other => {
            return Err(Error::refused(format!(
                "`git rev-parse --is-shallow-repository` in {} printed `{other}`",
                dir.display()
            )));
        }
    }
    let n = run(dir, &["rev-list", "--count", rev])?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
        .ok_or_else(|| {
            Error::refused(format!(
                "`git rev-list --count {rev}` in {} printed `{n}`",
                dir.display()
            ))
        })
}

/// The full sha `HEAD` names in `dir`.
/// # Errors
/// When git fails.
pub fn head(dir: &Path) -> Result<String> {
    run(dir, &["rev-parse", "HEAD"])
}

/// `git status --porcelain` with every untracked file and submodule change listed whatever
/// the repository's or the user's config says (`status.showUntrackedFiles=no` and
/// `diff.ignoreSubmodules` would otherwise hide them), plus `extra`.
const STATUS: [&str; 4] = [
    "status",
    "--porcelain",
    "--untracked-files=all",
    "--ignore-submodules=none",
];

/// Whether `dir` is a work tree with no change to a tracked or untracked file, or to a
/// submodule, whatever the config says.
/// # Errors
/// When `dir` is not a work tree.
pub fn is_clean(dir: &Path) -> Result<bool> {
    run(dir, &["rev-parse", "--is-inside-work-tree"])?;
    Ok(run(dir, &STATUS)?.is_empty())
}

/// The first path that keeps `dir` from being a fresh clone's tree, or `None`: a change to a
/// tracked or untracked file or a submodule ([`is_clean`]), or an ignored file anywhere but the
/// top-level `target/` (an ignored `.cargo/config.toml`, `build.rs` or source file is read by
/// a build exactly as a tracked one is, and an ignore rule can come from the user's own
/// `core.excludesFile`). Index hints that can hide tracked changes also prevent publication;
/// this query never clears them in the caller's checkout.
/// # Errors
/// When `dir` is not a work tree.
pub fn is_pristine(dir: &Path) -> Result<Option<String>> {
    run(dir, &["rev-parse", "--is-inside-work-tree"])?;
    // `-v` lowercases assume-unchanged entries; `S` names skip-worktree entries.
    // A clean status is not evidence about the contents Git was told not to inspect.
    for args in [
        &["ls-files", "-v", "-z"][..],
        &["ls-files", "-v", "-z", "--recurse-submodules"][..],
    ] {
        let entries = run(dir, args)?;
        if let Some(entry) = entries.split('\0').find(|entry| {
            entry
                .as_bytes()
                .first()
                .is_some_and(|flag| flag.is_ascii_lowercase() || *flag == b'S')
        }) {
            return Ok(Some(format!("index hint hides tracked input: {entry:?}")));
        }
    }
    let mut args = STATUS.to_vec();
    args.push("--ignored=matching");
    let status = run(dir, &args)?;
    Ok(status
        .lines()
        .find(|line| *line != "!! target/")
        .map(str::to_string))
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
