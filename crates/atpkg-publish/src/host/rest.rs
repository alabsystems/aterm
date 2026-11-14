// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`RestHost`]: GitHub REST API and git implementation of [`ReleaseHost`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::Arc;
use std::time::Duration;

use aterm_json::Value;
use aterm_types::http_retry::Policy;
use aterm_uds::spawnfd::Command;
#[cfg(any(test, debug_assertions))]
use aterm_update_core::github_rest::Spawn;
use aterm_update_core::github_rest::{Answer, Client, Method, Origins, RestError, Token};
use aterm_update_core::release_asset::{Asset, AssetError, Reader};

use super::{HostError, Probe, RETRY_PAUSE, Release, ReleaseHost, Replace, STAGED, Slug};
use crate::git::{
    GIT_TLS_PINS, git_environment_removals, git_tls_pins_at, git_tracing_environment,
};

const PER_PAGE: usize = 100;
/// An anonymous Git child that produces neither captured bytes nor descendant CPU/I/O progress
/// for this long is hung. Progress starts another full window; this is not a latency budget.
const ANONYMOUS_GIT_STALL: Duration = Duration::from_secs(60);

/// Environment variable from which git's credential helper reads the token.
pub const GIT_TOKEN_ENV: &str = "ATPKG_PUBLISH_GIT_TOKEN";

/// An injected Git network attempt. Production owns the real child in [`RestHost::git_attempt`];
/// tests replace only this leaf so selection, retry and clone publication remain live code.
#[cfg(any(test, debug_assertions))]
trait GitTransport: Send + Sync {
    fn run(
        &self,
        args: &[OsString],
        trace: aterm_update_core::git_https::Trace<'_>,
        waiting_on: &str,
    ) -> std::result::Result<(), aterm_update_core::git_https::Failure>;
}

/// Credentials for the known Git helper. The secret is deliberately absent from argv and Debug.
pub(crate) struct GitAuth {
    token: Option<Token>,
    origin: Option<String>,
}

impl std::fmt::Debug for GitAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitAuth")
            .field("origin", &self.origin)
            .field("authenticated", &self.token.is_some())
            .finish()
    }
}

impl GitAuth {
    fn new(token: Option<Token>, base: &str) -> Self {
        Self {
            token,
            origin: credential_origin(base).map(str::to_owned),
        }
    }

    pub(crate) fn arguments(&self) -> Vec<OsString> {
        let mut args = vec!["-c".into(), "credential.helper=".into()];
        if self.token.is_some()
            && let Some(origin) = &self.origin
        {
            args.extend([
                "-c".into(),
                format!("credential.{origin}.helper={}", git_helper()).into(),
            ]);
        }
        args
    }

    /// The known helper's child environment; callers never log these values.
    /// # Errors
    /// A token whose bytes are not UTF-8.
    pub(crate) fn environment(&self) -> Result<Vec<(String, OsString)>, HostError> {
        match (&self.token, &self.origin) {
            (Some(token), Some(_)) => {
                let text = std::str::from_utf8(token.expose())
                    .map_err(|_| HostError::Refused("the token is not UTF-8".into()))?;
                Ok(vec![(GIT_TOKEN_ENV.into(), text.into())])
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// GitHub's REST API, as one identity.
#[derive(Clone)]
pub struct RestHost {
    /// The token file this identity reads; `None` reads anonymously and writes nothing.
    pub token_file: Option<PathBuf>,
    origins: Origins,
    /// Where git finds `{owner}/{repo}.git`: the web origin, or a test's server or directory
    /// of bare repositories.
    git_base: String,
    retry_pause: Duration,
    /// How long a public read goes on without progress ([`aterm_update_core::release_asset`]).
    read_patience: Duration,
    /// Tests: what starts curl (a recorder of each spawn).
    #[cfg(any(test, debug_assertions))]
    spawn: Option<Arc<dyn Spawn>>,
    /// Tests: variables set on each git (a scratch global config).
    #[cfg(any(test, debug_assertions))]
    git_env: Vec<(OsString, OsString)>,
    /// Tests: one injected Git network attempt; policy and staging stay in this host.
    #[cfg(any(test, debug_assertions))]
    git_transport: Option<Arc<dyn GitTransport>>,
}

impl std::fmt::Debug for RestHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestHost")
            .field("token_file", &self.token_file)
            .field("origins", &self.origins)
            .field("git_base", &self.git_base)
            .finish_non_exhaustive()
    }
}

impl RestHost {
    /// github.com, speaking as the token in `token_file`, or anonymously.
    #[must_use]
    pub fn new(token_file: Option<PathBuf>) -> Self {
        let origins = Origins::github();
        Self {
            token_file,
            git_base: origins.web.clone(),
            origins,
            retry_pause: RETRY_PAUSE,
            read_patience: aterm_update_core::release_asset::PATIENCE,
            #[cfg(any(test, debug_assertions))]
            spawn: None,
            #[cfg(any(test, debug_assertions))]
            git_env: Vec::new(),
            #[cfg(any(test, debug_assertions))]
            git_transport: None,
        }
    }

    /// Constructs a host for custom `origins` and `git_base` with zero retry pause, whose public
    /// reads try each route once.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn at(token_file: Option<PathBuf>, origins: Origins, git_base: String) -> Self {
        Self {
            token_file,
            origins,
            git_base,
            retry_pause: Duration::ZERO,
            read_patience: Duration::ZERO,
            spawn: None,
            git_env: Vec::new(),
            git_transport: None,
        }
    }

    /// Tests: the same host, starting each curl through `spawn` (a recorder).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_spawn(self, spawn: Arc<dyn Spawn>) -> Self {
        Self {
            spawn: Some(spawn),
            ..self
        }
    }

    /// Sets extra environment variables on every spawned `git` command.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_git_env(self, vars: &[(&str, &std::ffi::OsStr)]) -> Self {
        Self {
            git_env: vars
                .iter()
                .map(|(k, v)| (OsString::from(k), v.to_os_string()))
                .collect(),
            ..self
        }
    }

    fn token(&self) -> Result<Option<Token>, HostError> {
        match &self.token_file {
            None => Ok(None),
            Some(file) => Token::from_file(file).map_err(HostError::Refused),
        }
    }

    fn client(&self, token: Option<Token>) -> Client {
        let client = Client::new(self.origins.clone(), token);
        #[cfg(any(test, debug_assertions))]
        let client = if self.retry_pause.is_zero() {
            client.without_retry_wait()
        } else {
            client
        };
        #[cfg(any(test, debug_assertions))]
        let client = match &self.spawn {
            Some(spawn) => client.with_spawn(Arc::clone(spawn)),
            None => client,
        };
        client
    }

    fn reader(&self) -> Result<Client, HostError> {
        Ok(self.client(self.token()?))
    }

    fn writer(&self) -> Result<Client, HostError> {
        match self.token()? {
            Some(token) => Ok(self.client(Some(token))),
            None => Err(HostError::Refused(
                "no token file is configured for this owner (it was absent or empty when the \
                 run started), and a write never goes anonymously"
                    .to_string(),
            )),
        }
    }

    /// One credential-less repository read under the fleet's anonymous HTTPS budget.
    fn anonymous_repo_seen(&self, path: &str) -> Result<bool, HostError> {
        let client = self.client(None);
        let answer = aterm_types::http_retry::run(
            Policy::STANDARD,
            |_| match client.read(path) {
                Ok(_) => Ok(true),
                Err(error) if error.is_not_found() => Ok(false),
                Err(error) => Err(error),
            },
            RestError::retry_cause,
            |retry| {
                eprintln!("    anonymous GitHub repository read: {retry}");
                // Fixture hosts use a zero pause; production follows the shared policy exactly.
                if !self.retry_pause.is_zero() {
                    std::thread::sleep(retry.pause);
                }
            },
        );
        answer.map_err(|failure| {
            if failure.exhausted {
                HostError::Unreachable(format!(
                    "anonymous GitHub repository read failed after {}: {}",
                    failure.bound, failure.error
                ))
            } else {
                host_error(&failure.error)
            }
        })
    }

    /// One successful API GET. A tokenless client owns the fleet-wide anonymous-read budget;
    /// credentialed/private reads retain their existing single request here.
    fn read_answer(&self, client: &Client, path: &str) -> Result<Answer, RestError> {
        if client.has_token() {
            return client.get(path)?.success();
        }
        let answer = aterm_types::http_retry::run(
            Policy::STANDARD,
            |_| client.get(path).and_then(Answer::success),
            RestError::retry_cause,
            |retry| {
                eprintln!("    anonymous GitHub API read: {retry}");
                if !self.retry_pause.is_zero() {
                    std::thread::sleep(retry.pause);
                }
            },
        );
        answer.map_err(|failure| {
            if failure.exhausted {
                RestError::Transport {
                    exit: failure.error.curl_exit(),
                    status: failure.error.status(),
                    retry_after_secs: failure.error.retry_after_secs(),
                    message: format!(
                        "anonymous GitHub API read failed after {}: {}",
                        failure.bound, failure.error
                    ),
                }
            } else {
                failure.error
            }
        })
    }

    fn read_value(&self, client: &Client, path: &str) -> Result<Value, RestError> {
        self.read_answer(client, path)?.json()
    }

    fn object(&self, client: &Client, slug: &Slug, tag: &str) -> Result<Value, HostError> {
        let by_tag = format!("/repos/{slug}/releases/tags/{}", segment(tag));
        match self.read_value(client, &by_tag) {
            Ok(object) => return Ok(object),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(host_error(&e)),
        }
        let drafts: Vec<Value> = self
            .listing(client, slug)?
            .into_iter()
            .filter(|r| r["tag_name"].as_str() == Some(tag) && r["draft"].as_bool() == Some(true))
            .collect();
        match <[Value; 1]>::try_from(drafts) {
            Ok([draft]) => Ok(draft),
            Err(drafts) if drafts.is_empty() => Err(HostError::NotFound),
            Err(drafts) => Err(HostError::Refused(format!(
                "ambiguous: {} drafts have tag {tag} on {slug}",
                drafts.len()
            ))),
        }
    }

    fn listing(&self, client: &Client, slug: &Slug) -> Result<Vec<Value>, HostError> {
        let base = format!("/repos/{slug}/releases?per_page={PER_PAGE}");
        let mut failed = None;
        let listed = aterm_update_core::github_pages::all(
            &client.origins().api,
            &base,
            PER_PAGE,
            |endpoint| match self.read_answer(client, endpoint) {
                Ok(answer) => aterm_update_core::github_pages::page(
                    answer.status,
                    answer.headers,
                    &answer.body,
                ),
                Err(error) => {
                    let words = error.to_string();
                    failed = Some(error);
                    Err(words)
                }
            },
        )
        .map_err(|why| failed.take().unwrap_or(RestError::Malformed(why)));
        listed.map_err(|e| match e {
            RestError::Status { status: 404, .. } => {
                HostError::Unreachable(format!("{slug} is not visible to this identity ({e})"))
            }
            e => host_error(&e),
        })
    }

    fn id_of(object: &Value) -> Result<u64, HostError> {
        object["id"]
            .as_u64()
            .ok_or_else(|| HostError::Unreachable("GitHub answered a release with no id".into()))
    }

    fn publish(client: &Client, slug: &Slug, id: u64) -> Result<(), HostError> {
        let body = aterm_json::json!({"draft": false, "prerelease": true, "make_latest": "false"});
        client
            .send(
                Method::Patch,
                &format!("/repos/{slug}/releases/{id}"),
                Some(&body),
            )
            .and_then(|a| a.success())
            .map(|_| ())
            .map_err(|e| host_error(&e))
    }

    fn upload_one(
        client: &Client,
        slug: &Slug,
        id: u64,
        file: &Path,
        name: &str,
    ) -> Result<u64, HostError> {
        let url = client.uploads(&format!(
            "/repos/{slug}/releases/{id}/assets?name={}",
            segment(name)
        ));
        let asset = client
            .upload(&url, file)
            .and_then(|a| a.success())
            .and_then(|a| a.json())
            .map_err(|e| host_error(&e))?;
        asset["id"].as_u64().ok_or_else(|| {
            HostError::Unreachable(format!("GitHub answered the upload of {name} with no id"))
        })
    }

    fn delete_asset(client: &Client, slug: &Slug, asset: u64) -> Result<(), HostError> {
        let gone = client
            .send(
                Method::Delete,
                &format!("/repos/{slug}/releases/assets/{asset}"),
                None,
            )
            .and_then(|a| a.success());
        match gone {
            Ok(_) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(host_error(&e)),
        }
    }

    /// Before a clobber of `name`: its staged copy (`{name}.atpkg-staged`) an earlier replace
    /// left, and every temporary an `AghHost` clobber of the same name left
    /// (`{name}.agh-{8 hex}`, `agh::github::releases::is_clobber_temporary_of`), are deleted.
    /// During the swap both hosts write the same staging releases, and this clobber leaves
    /// `name` whole whatever the other host left, so either temporary would stay on the
    /// release, an asset the mirror refuses to republish.
    fn drop_staged(
        client: &Client,
        slug: &Slug,
        object: &Value,
        name: &str,
    ) -> Result<(), HostError> {
        match asset_of(object, &format!("{name}{STAGED}")) {
            Ok((staged, _)) => Self::delete_asset(client, slug, staged)?,
            Err(HostError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let agh_temporaries = object["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|asset| {
                asset["name"].as_str().is_some_and(|asset| {
                    agh::github::releases::is_clobber_temporary_of(asset, name)
                })
            })
            .filter_map(|asset| asset["id"].as_u64());
        for temporary in agh_temporaries {
            Self::delete_asset(client, slug, temporary)?;
        }
        Ok(())
    }

    fn replace_one(
        client: &Client,
        slug: &Slug,
        object: &Value,
        id: u64,
        old: u64,
        file: &Path,
        name: &str,
    ) -> Result<(), HostError> {
        Self::drop_staged(client, slug, object, name)?;
        let staged = Self::upload_one(client, slug, id, file, &format!("{name}{STAGED}"))?;
        Self::delete_asset(client, slug, old)?;
        client
            .send(
                Method::Patch,
                &format!("/repos/{slug}/releases/assets/{staged}"),
                Some(&aterm_json::json!({ "name": name })),
            )
            .and_then(|a| a.success())
            .map(|_| ())
            .map_err(|e| host_error(&e))
    }

    /// [`Self::git_output`], its output dropped.
    fn git(&self, args: &[OsString], url: Option<&str>) -> Result<(), HostError> {
        self.git_output(args, url).map(|_| ())
    }

    /// One canonical tokenless Git-over-HTTPS read under the shared anonymous budget. A
    /// credentialed or custom-origin operation retains the existing transport.
    fn git_network(&self, args: &[OsString], url: &str, waiting_on: &str) -> Result<(), HostError> {
        if !self.canonical_anonymous_git(url)? {
            return self.git(args, Some(url));
        }
        anonymous_git_read(
            |_, trace| self.git_attempt(args, url, trace, waiting_on),
            |retry| {
                if !self.retry_pause.is_zero() {
                    std::thread::sleep(retry.pause);
                }
            },
        )
    }

    /// Run one anonymous Git attempt. Git/libcurl owns its low-speed detector; the outer process
    /// watcher ends only a full inactivity window with no captured or descendant progress.
    fn git_attempt(
        &self,
        args: &[OsString],
        url: &str,
        trace: aterm_update_core::git_https::Trace<'_>,
        waiting_on: &str,
    ) -> std::result::Result<(), aterm_update_core::git_https::Failure> {
        #[cfg(any(test, debug_assertions))]
        if let Some(transport) = &self.git_transport {
            return transport.run(args, trace, waiting_on);
        }
        let mut command = self
            .git_command(args, Some(url))
            .map_err(|error| aterm_update_core::git_https::Failure::local(error.to_string()))?;
        trace.configure(&mut command);
        let out = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
            &mut command,
            ANONYMOUS_GIT_STALL,
            waiting_on,
        )
        .map_err(|error| match error {
            aterm_uds::deadline::CompleteProgressError::Stalled { .. } => {
                aterm_update_core::git_https::Failure::timeout(error.to_string())
            }
            aterm_uds::deadline::CompleteProgressError::Complete(
                aterm_uds::deadline::CompleteOutputError::Run(
                    aterm_uds::deadline::RunError::Spawn(error),
                ),
            ) => aterm_update_core::git_https::Failure::local(format!("spawn git: {error}")),
            other => aterm_update_core::git_https::Failure::local(format!("git: {other}")),
        })?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(aterm_update_core::git_https::Failure::diagnostic(
            if stderr.is_empty() {
                format!("git exited {:?}", out.status.code())
            } else {
                stderr
            },
        ))
    }

    /// Whether this exact network URL is the production public route with no usable token.
    fn canonical_anonymous_git(&self, url: &str) -> Result<bool, HostError> {
        Ok(self.token()?.is_none()
            && self.origins.web == "https://github.com"
            && self.git_base == self.origins.web
            && url
                .strip_prefix("https://github.com/")
                .is_some_and(|path| !path.is_empty())
            && aterm_update_core::git_https::is_anonymous_https_url(url))
    }

    /// `git <args…>` ([`Self::git_command`]) within [`super::agh::LANE_GIT_BUDGET`], the bound a
    /// lane's clone or fetch through agh has ([`crate::git::output_within`]).
    fn git_output(
        &self,
        args: &[OsString],
        url: Option<&str>,
    ) -> Result<std::process::Output, HostError> {
        let out = crate::git::output_within(
            &mut self.git_command(args, url)?,
            super::agh::LANE_GIT_BUDGET,
        )
        .map_err(|e| match e {
            aterm_uds::deadline::CompleteOutputError::Run(
                aterm_uds::deadline::RunError::Spawn(e),
            ) => HostError::Unreachable(format!("spawn git: {e}")),
            other => HostError::Unreachable(format!("git: {other}")),
        })?;
        if out.status.success() {
            return Ok(out);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(HostError::Unreachable(format!(
            "git: {}",
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        )))
    }

    /// `git <args…>` with this host's auth and agh's TLS pins: the unscoped [`GIT_TLS_PINS`],
    /// and for a network git the same pins at `url`, the remote it is given
    /// ([`git_tls_pins_at`]), which hold over a key the user's, the system's or the checkout's
    /// config scopes to a URL it matches. An inherited command-scope config variable is
    /// removed with the rest of [`git_environment_removals`]. A `url.*.insteadOf` rewrite in
    /// that config (`fetch_main` lets a checkout's mirror transport objects) is followed, and an
    /// HTTP URL it rewrites to has the unscoped pins alone.
    fn git_command(&self, args: &[OsString], url: Option<&str>) -> Result<Command, HostError> {
        let auth = GitAuth::new(self.token()?, &self.git_base);
        let scoped = url
            .map(git_tls_pins_at)
            .transpose()
            .map_err(HostError::Refused)?
            .unwrap_or_default();
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        cmd.args(auth.arguments()).args(GIT_TLS_PINS).args(scoped);
        for name in git_environment_removals() {
            cmd.env_remove(name);
        }
        for (name, value) in git_tracing_environment()
            .into_iter()
            .chain(auth.environment()?)
        {
            cmd.env(name, value);
        }
        #[cfg(any(test, debug_assertions))]
        for (name, value) in &self.git_env {
            cmd.env(name, value);
        }
        cmd.args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "");
        Ok(cmd)
    }

    fn git_url(&self, slug: &Slug) -> String {
        format!("{}/{slug}.git", self.git_base)
    }
}

/// One injected anonymous Git read. The shared helper logs every retry; the caller supplies its
/// clock so fixture transports spend no wall time.
fn anonymous_git_read<T>(
    attempt: impl FnMut(
        u32,
        aterm_update_core::git_https::Trace<'_>,
    ) -> std::result::Result<T, aterm_update_core::git_https::Failure>,
    on_retry: impl FnMut(aterm_types::http_retry::Retry),
) -> Result<T, HostError> {
    aterm_update_core::git_https::read(true, attempt, on_retry).map_err(|failure| {
        HostError::Unreachable(format!("anonymous Git HTTPS read failed: {failure}"))
    })
}

/// Publish a complete clone only if `dest` is still absent. Unix uses the workspace's kernel
/// NOREPLACE primitive, so a concurrent creator is never replaced.
#[cfg(unix)]
fn publish_clone_noreplace(from: &Path, dest: &Path) -> std::io::Result<()> {
    aterm_dirfd::renameat_with(
        aterm_dirfd::CWD,
        from,
        aterm_dirfd::CWD,
        dest,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn publish_clone_noreplace(from: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "clone destination already exists",
        ));
    }
    std::fs::rename(from, dest)
}

fn credential_origin(base: &str) -> Option<&str> {
    let (scheme, host) = base.split_once("://")?;
    (matches!(scheme, "https" | "http")
        && !host.is_empty()
        && host.bytes().all(|b| {
            b.is_ascii_graphic() && !matches!(b, b'/' | b'\\' | b'?' | b'#' | b'@' | b'=' | b'"')
        }))
    .then_some(base)
}

fn git_helper() -> String {
    format!(
        "!f() {{ if test \"$1\" = get; then printf 'username=x-access-token\\npassword=%s\\n' \
         \"${GIT_TOKEN_ENV}\"; fi; }}; f"
    )
}

fn segment(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn file_name(file: &Path) -> Result<&str, HostError> {
    file.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| HostError::Refused(format!("{} has no asset name", file.display())))
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.ends_with(PART)
}

const PART: &str = ".atpkg-part";

fn host_error(e: &RestError) -> HostError {
    match e {
        RestError::Status { status: 404, .. } => HostError::NotFound,
        RestError::Status { message, .. } if message.contains("already_exists") => {
            HostError::Refused(e.to_string())
        }
        RestError::Local(why) => HostError::Refused(why.clone()),
        _ => HostError::Unreachable(e.to_string()),
    }
}

fn release_of(object: &Value) -> Result<Release, HostError> {
    let unreadable = || {
        HostError::Unreachable(format!(
            "GitHub answered a release this cannot read: {object}"
        ))
    };
    let tag = object["tag_name"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(unreadable)?;
    let draft = object["draft"].as_bool().ok_or_else(unreadable)?;
    let prerelease = object["prerelease"].as_bool().ok_or_else(unreadable)?;
    let assets = object["assets"]
        .as_array()
        .ok_or_else(unreadable)?
        .iter()
        .map(|a| {
            a["name"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(unreadable)
        })
        .collect::<Result<_, _>>()?;
    Ok(Release {
        tag: tag.to_string(),
        draft,
        prerelease,
        published_at: object["published_at"].as_str().map(str::to_string),
        assets,
    })
}

fn asset_of(object: &Value, name: &str) -> Result<(u64, u64), HostError> {
    let mut named = object["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["name"].as_str() == Some(name));
    let Some(asset) = named.next() else {
        return Err(HostError::NotFound);
    };
    let extra = named.count();
    if extra > 0 {
        return Err(HostError::Refused(format!(
            "ambiguous: {} assets are named {name}",
            extra + 1
        )));
    }
    match (asset["id"].as_u64(), asset["size"].as_u64()) {
        (Some(id), Some(size)) => Ok((id, size)),
        _ => Err(HostError::Unreachable(format!(
            "GitHub answered asset {name} with no id or size"
        ))),
    }
}

impl RestHost {
    fn create_release(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
        publish: bool,
    ) -> Result<(), HostError> {
        let client = self.writer()?;
        for file in files {
            file_name(file)?;
        }
        let body = aterm_json::json!({
            "tag_name": tag,
            "name": title,
            "body": notes,
            "draft": true,
            "prerelease": true,
            "make_latest": "false",
        });
        let made = client
            .send(
                Method::Post,
                &format!("/repos/{slug}/releases"),
                Some(&body),
            )
            .and_then(|a| a.success())
            .and_then(|a| a.json())
            .map_err(|e| host_error(&e))?;
        let id = Self::id_of(&made)?;
        for file in files {
            Self::upload_one(&client, slug, id, file, file_name(file)?)?;
        }
        if publish {
            Self::publish(&client, slug, id)
        } else {
            Ok(())
        }
    }
}

impl ReleaseHost for RestHost {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        let client = self.reader()?;
        self.listing(&client, slug)?
            .iter()
            .map(release_of)
            .collect()
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        let client = self.reader()?;
        release_of(&self.object(&client, slug, tag)?)
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        if let Some(bad) = assets.iter().find(|a| !plain_name(a)) {
            return Err(HostError::Refused(format!(
                "{bad:?} is not a plain asset name; nothing was downloaded"
            )));
        }
        let client = self.reader()?;
        let object = self.object(&client, slug, tag)?;
        let wanted = assets
            .iter()
            .map(|name| asset_of(&object, name).map(|found| (*name, found)))
            .collect::<Result<Vec<_>, _>>()?;
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Refused(format!("mkdir {}: {e}", dir.display())))?;
        let staged = aterm_tempfile::Builder::new()
            .prefix(".atpkg-release-download-")
            .tempdir_in(dir)
            .map_err(|e| {
                HostError::Refused(format!(
                    "make a private download stage in {}: {e}",
                    dir.display()
                ))
            })?;
        for &(name, (id, size)) in &wanted {
            let part = staged.path().join(name);
            let path = format!("/repos/{slug}/releases/assets/{id}");
            client
                .download_asset(&path, &part, size.max(1))
                .map_err(|e| host_error(&e))?;
            let got = std::fs::symlink_metadata(&part)
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map_or(u64::MAX, |m| m.len());
            if got != size {
                return Err(HostError::Unreachable(format!(
                    "{slug} {tag} {name}: GitHub lists {size} bytes, the download holds {got}"
                )));
            }
        }
        let mut published = std::collections::BTreeSet::new();
        for (name, _) in wanted {
            if !published.insert(name) {
                continue;
            }
            std::fs::rename(staged.path().join(name), dir.join(name)).map_err(|e| {
                HostError::Refused(format!("place {name} in {}: {e}", dir.display()))
            })?;
        }
        Ok(())
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.create_release(slug, tag, title, notes, files, true)
    }

    fn create_draft(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.create_release(slug, tag, title, notes, files, false)
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        let client = self.writer()?;
        for file in files {
            if file_name(file)?.ends_with(STAGED) {
                return Err(HostError::Refused(format!(
                    "{} ends in {STAGED}, the name a replace stages under",
                    file.display()
                )));
            }
        }
        let object = self.object(&client, slug, tag)?;
        let id = Self::id_of(&object)?;
        for file in files {
            let name = file_name(file)?;
            match (asset_of(&object, name), replace) {
                (Err(HostError::NotFound), Replace::Never) => {
                    Self::upload_one(&client, slug, id, file, name)?;
                }
                (Err(HostError::NotFound), Replace::Clobber) => {
                    Self::drop_staged(&client, slug, &object, name)?;
                    Self::upload_one(&client, slug, id, file, name)?;
                }
                (Ok(_), Replace::Never) => {
                    return Err(HostError::Refused(format!(
                        "{slug} {tag} already has an asset {name} (already_exists)"
                    )));
                }
                (Ok((old, _)), Replace::Clobber) => {
                    Self::replace_one(&client, slug, &object, id, old, file, name)?;
                }
                (Err(e), _) => return Err(e),
            }
        }
        Ok(())
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        let client = self.writer()?;
        let object = self.object(&client, slug, tag)?;
        Self::publish(&client, slug, Self::id_of(&object)?)
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        match aterm_update_core::head_no_redirect(&self.public_url(slug, tag, asset)) {
            Ok(answer) => probe_of(answer.code),
            Err(e) => Probe::Unknown(format!("no answer from the download host: {e:?}")),
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(format!("mkdir {}: {e}", parent.display()));
        }
        // The shared release reader: the web route as clients take it, then the API route when
        // the download host fails, whose requests carry this identity's token (its own
        // allowance, not the address's shared anonymous one); the web and CDN legs stay
        // anonymous.
        let reader = Reader::new(self.origins.clone(), self.token().ok().flatten())
            .patient(self.read_patience);
        let named = Asset::tagged(&slug.owner, &slug.repo, tag, asset);
        // Every read starts from the first byte: the reader continues a partial file it finds,
        // and an asset replaced under its tag must not continue an earlier upload's bytes.
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let _ = std::fs::remove_file(&part);
        // The reader writes `dest` only by renaming a whole `.part` onto it, so a read that did
        // not deliver leaves the caller's file as it was.
        let answer = match reader.to_file(&named, dest, cap) {
            Ok(()) => return Probe::Present,
            Err(AssetError::Absent(_)) => Probe::Absent,
            Err(e) => Probe::Unknown(e.to_string()),
        };
        let _ = std::fs::remove_file(&part);
        answer
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        let url = self.git_url(slug);
        if !self.canonical_anonymous_git(&url)? {
            let mut args: Vec<OsString> = vec!["clone".into(), "--quiet".into(), "--".into()];
            args.push(url.clone().into());
            args.push(dest.as_os_str().to_owned());
            return self.git(&args, Some(&url));
        }
        match std::fs::symlink_metadata(dest) {
            Ok(_) => {
                return Err(HostError::Refused(format!(
                    "clone destination {} already exists",
                    dest.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(HostError::Refused(format!(
                    "inspect clone destination {}: {error}",
                    dest.display()
                )));
            }
        }
        let parent = dest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let stage = aterm_tempfile::Builder::new()
            .prefix(".atpkg-anonymous-clone-")
            .tempdir_in(parent)
            .map_err(|error| {
                HostError::Refused(format!(
                    "make private clone stage beside {}: {error}",
                    dest.display()
                ))
            })?;
        let cloned = anonymous_git_read(
            |attempt, trace| {
                let attempt_dest = stage.path().join(format!("attempt-{attempt}"));
                let args: Vec<OsString> = vec![
                    "clone".into(),
                    "--quiet".into(),
                    "--".into(),
                    url.clone().into(),
                    attempt_dest.as_os_str().to_owned(),
                ];
                self.git_attempt(&args, &url, trace, "anonymous GitHub clone")?;
                Ok(attempt_dest)
            },
            |retry| {
                if !self.retry_pause.is_zero() {
                    std::thread::sleep(retry.pause);
                }
            },
        )?;
        publish_clone_noreplace(&cloned, dest).map_err(|error| {
            HostError::Refused(format!(
                "publish clone at {} without replacing another entry: {error}",
                dest.display()
            ))
        })
    }

    fn retry_pause(&self) -> Duration {
        self.retry_pause
    }

    fn download_owns_retry(&self) -> bool {
        true
    }

    fn reads_anonymously(&self) -> bool {
        matches!(self.token(), Ok(None))
    }

    fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
        let path = format!("/repos/{slug}");
        let ask = |client: Client| match client.read(&path) {
            Ok(_) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(host_error(&e)),
        };
        if let Some(token) = self.token()?
            && ask(self.client(Some(token)))?
        {
            return Ok(true);
        }
        self.anonymous_repo_seen(&path)
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        let url = self.git_url(slug);
        let mut args: Vec<OsString> = vec!["-C".into(), dir.as_os_str().to_owned()];
        args.extend(
            [
                "fetch",
                "--quiet",
                "--tags",
                "--force",
                "--recurse-submodules=no",
            ]
            .iter()
            .map(OsString::from),
        );
        args.push(url.clone().into());
        args.push("+refs/heads/*:refs/remotes/origin/*".into());
        self.git_network(&args, &url, "anonymous GitHub repository fetch")
            .map_err(|e| match e {
                HostError::Unreachable(why) => {
                    HostError::Unreachable(format!("fetch of {slug}: {why}"))
                }
                other => other,
            })
    }

    fn fetch_main(&self, slug: &Slug, dir: &Path, into: &str) -> Result<(), HostError> {
        // Git may rewrite even an explicit host URL to the checkout's local mirror. Let it
        // transport objects, but only the independently read API may name the main of record.
        let main = self
            .reader()
            .and_then(|client| {
                self.read_value(&client, &format!("/repos/{slug}/commits/refs/heads/main"))
                    .map_err(|e| host_error(&e))
            })
            .map_err(|why| {
                HostError::Unreachable(format!("read {slug}'s main from the host API: {why}"))
            })?;
        let expected = main["sha"]
            .as_str()
            .filter(|sha| {
                sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or_else(|| {
                HostError::Unreachable(format!(
                    "the host API answered {slug}'s main without a full lowercase commit SHA"
                ))
            })?;
        let url = self.git_url(slug);
        let mut args: Vec<OsString> = vec!["-C".into(), dir.as_os_str().to_owned()];
        args.extend(
            [
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--recurse-submodules=no",
            ]
            .iter()
            .map(OsString::from),
        );
        args.push(url.clone().into());
        args.push(format!("+refs/heads/main:{into}").into());
        self.git_network(&args, &url, "anonymous GitHub main fetch")
            .map_err(|e| match e {
                HostError::Unreachable(why) => {
                    HostError::Unreachable(format!("fetch of {slug}'s main: {why}"))
                }
                other => other,
            })?;
        let fetched = self.git_output(
            &[
                "-C".into(),
                dir.as_os_str().to_owned(),
                "rev-parse".into(),
                "--verify".into(),
                into.into(),
            ],
            None,
        );
        let checked = fetched.and_then(|out| {
            let actual = String::from_utf8_lossy(&out.stdout);
            if actual.trim() == expected {
                Ok(())
            } else {
                Err(HostError::Unreachable(format!(
                    "fetched {slug}'s main ({}) differs from the host API ({expected}); \
                     main moved or git transported another main",
                    actual.trim()
                )))
            }
        });
        if let Err(why) = checked {
            let dropped = self.git(
                &[
                    "-C".into(),
                    dir.as_os_str().to_owned(),
                    "update-ref".into(),
                    "-d".into(),
                    into.into(),
                ],
                None,
            );
            return Err(HostError::Unreachable(match dropped {
                Ok(()) => format!("could not bind {slug}'s main of record: {why}"),
                Err(drop) => format!(
                    "could not bind {slug}'s main of record: {why}; removing {into} failed: {drop}"
                ),
            }));
        }
        Ok(())
    }
}

impl RestHost {
    fn public_url(&self, slug: &Slug, tag: &str, asset: &str) -> String {
        format!(
            "{}/{}/{}/releases/download/{tag}/{asset}",
            self.origins.web, slug.owner, slug.repo
        )
    }
}

/// Maps an HTTP status code from the download host to a [`Probe`].
#[must_use]
pub fn probe_of(code: u16) -> Probe {
    match code {
        200 | 302 => Probe::Present,
        404 => Probe::Absent,
        other => Probe::Unknown(format!("the download host answered HTTP {other}")),
    }
}

#[cfg(test)]
#[path = "rest_tests.rs"]
mod tests;
