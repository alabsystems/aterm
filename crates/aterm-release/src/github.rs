// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub's REST API for the release cutter, routed through [`aterm_update_core::github_rest`].

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use aterm_json::Value;
use aterm_types::http_retry::{Budget, Cause, Decision, Policy};
use aterm_uds::spawnfd::{Command, Stdio};
use aterm_update_core::github_rest::{
    Answer, Body, Client, Method, Origins, Prepared, Request, RestError, Spawn, Token,
};

use crate::ledger::{Error, Result, RunOut};

/// The `Accept` of a raw content read (`/repos/{slug}/contents/{path}`): the file's bytes, not its
/// JSON envelope.
pub(crate) const ACCEPT_RAW: &str = "application/vnd.github.raw";

/// The bounded policy shared by anonymous HTTPS reads in the cutter and verifier.
const READ_RETRY_POLICY: Policy = Policy::STANDARD;

/// The cutter's pre-existing bound for idempotent authenticated mutations. Anonymous reads get
/// the incident-sized budget above; broadening a metadata PATCH or guarded DELETE to nineteen
/// attempts is neither necessary nor part of the anonymous-read policy.
const AUTHENTICATED_RETRY_POLICY: Policy = Policy::new(
    3,
    [Duration::from_secs(2), Duration::from_secs(5)],
    Duration::from_secs(5),
    Duration::from_secs(7),
);
const AUTHENTICATED_BACKOFFS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];

/// A test's GitHub: agh-fake's origins, the token (if any), and the spawn curl runs through.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Fixture {
    pub(crate) origins: Origins,
    pub(crate) token: Option<String>,
    pub(crate) spawn: Option<std::sync::Arc<dyn aterm_update_core::github_rest::Spawn>>,
}

#[cfg(test)]
thread_local! {
    static FIXTURE: std::cell::RefCell<Option<Fixture>> = const { std::cell::RefCell::new(None) };
}

/// While held, this thread's GitHub calls go to the fixture, and [`retried`] does not pause.
#[cfg(test)]
pub(crate) struct FixtureGuard(Option<Fixture>);

#[cfg(test)]
impl Fixture {
    /// Capture the caller's route before starting scoped request workers.
    pub(crate) fn current() -> Option<Self> {
        FIXTURE.with(|f| f.borrow().clone())
    }

    /// Route this thread's GitHub calls to `self` until the guard drops.
    pub(crate) fn enter(self) -> FixtureGuard {
        FixtureGuard(FIXTURE.with(|f| f.replace(Some(self))))
    }
}

#[cfg(test)]
impl Drop for FixtureGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        FIXTURE.with(|f| *f.borrow_mut() = previous);
    }
}

/// The client every call speaks through: the release org's token when its file holds one, else
/// anonymous.
/// # Errors
/// A token file that cannot be read, or holds a token [`Token::new`] refuses.
pub(crate) fn client() -> Result<Client> {
    client_via(None)
}

/// [`client`], its curls started through `spawn` when one is given ([`Launched`]).
fn client_via(spawn: Option<&Arc<dyn Spawn>>) -> Result<Client> {
    let client = base_client()?;
    Ok(match spawn {
        Some(spawn) => client.with_spawn(Arc::clone(spawn)),
        None => client,
    })
}

/// The client before any caller's spawn: a test's fixture, or github.com as the token file.
fn base_client() -> Result<Client> {
    #[cfg(test)]
    if let Some(fixture) = FIXTURE.with(|f| f.borrow().clone()) {
        let token = fixture
            .token
            .map(|t| Token::new(t.as_bytes()))
            .transpose()
            .map_err(Error::new)?;
        let client = Client::new(fixture.origins, token);
        return Ok(match fixture.spawn {
            Some(spawn) => client.with_spawn(spawn),
            None => client,
        });
    }
    let token = match crate::publish::channel_token_path() {
        Some(path) => Token::from_file(&path).map_err(Error::new)?,
        None => None,
    };
    Ok(Client::new(Origins::github(), token))
}

/// curl started as `L` starts a child (the release's `StepChildren`, or `ship site`'s `Plain`).
#[derive(Clone, Debug)]
pub(crate) struct Launched<L>(pub(crate) L);

impl<L: crate::site::Launch + Send + Sync> Spawn for Launched<L> {
    fn run(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, guard) = self
            .0
            .spawn(command)
            .map_err(aterm_update_core::github_rest::unstarted)?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let _ = pipe.write_all(bytes);
        }
        let out = child.wait_with_output();
        drop(guard);
        out
    }
}

/// Whether the release org's token is configured (its file holds one).
pub(crate) fn has_token() -> bool {
    client().is_ok_and(|c| c.has_token())
}

/// `path` as an API path: a leading `/`, as `repos/{slug}/…` is spelled throughout the
/// release.
fn api_path(path: &str) -> std::borrow::Cow<'_, str> {
    if path.starts_with('/') {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Owned(format!("/{path}"))
    }
}

/// The shared anonymous-read policy's transient cause, when `e` is one.
#[must_use]
pub(crate) fn retry_cause(e: &RestError) -> Option<Cause> {
    e.retry_cause()
}

/// Whether the cutter's pre-existing authenticated policy knows another attempt cannot help.
/// Anonymous reads intentionally use the narrower shared classifier in [`retry_cause`].
#[must_use]
fn authenticated_definitive(e: &RestError) -> bool {
    match e {
        RestError::Local(_) | RestError::Redirect(_) => true,
        RestError::Status { status, .. } => {
            (400..500).contains(status) && !matches!(status, 408 | 429)
        }
        RestError::Transport { .. } | RestError::Malformed(_) => false,
    }
}

/// Wait `duration` between attempts, ending early once an interrupt is caught (no wait under a
/// test's `Fixture`).
fn pause(duration: Duration) {
    #[cfg(test)]
    if FIXTURE.with(|f| f.borrow().is_some()) {
        return;
    }
    let until = crate::durable::deadline_after(duration);
    while crate::interrupt::caught().is_none() {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(std::time::Duration::from_millis(50)));
    }
}

/// `attempt`, made within [`READ_RETRY_POLICY`], `before_each` before every one.
/// # Errors
/// `before_each`'s; a final failure at once; an interrupt caught before an attempt; the last
/// transient failure once the bounded policy is exhausted.
pub(crate) fn retried<T>(
    what: &str,
    before_each: impl FnMut() -> Result<()>,
    attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    retried_via(None, what, before_each, attempt)
}

/// [`retried`], its curls started through `spawn` when one is given ([`Launched`]).
fn retried_via<T>(
    spawn: Option<&Arc<dyn Spawn>>,
    what: &str,
    before_each: impl FnMut() -> Result<()>,
    attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    let client = client_via(spawn)?;
    if client.has_token() {
        retried_authenticated_with_client(
            &client,
            AUTHENTICATED_RETRY_POLICY,
            what,
            before_each,
            attempt,
        )
    } else {
        retried_with_client(&client, READ_RETRY_POLICY, what, before_each, attempt)
    }
}

/// The common retry loop with the caller's operation-appropriate `policy`.
fn retried_via_policy<T>(
    spawn: Option<&Arc<dyn Spawn>>,
    policy: Policy,
    what: &str,
    before_each: impl FnMut() -> Result<()>,
    attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    let client = client_via(spawn)?;
    retried_authenticated_with_client(&client, policy, what, before_each, attempt)
}

/// The cutter's existing authenticated retry semantics. These deliberately remain broader than
/// the anonymous classifier: transport/malformed answers, HTTP 408 and headerless 429 get the
/// long-standing three attempts, while definitive 4xx and local refusals stop immediately.
fn retried_authenticated_with_client<T>(
    client: &Client,
    policy: Policy,
    what: &str,
    mut before_each: impl FnMut() -> Result<()>,
    mut attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    let attempts = policy.attempts();
    let mut last = String::new();
    for n in 1..=attempts {
        if let Some(signal) = crate::interrupt::caught() {
            let after = if last.is_empty() {
                String::new()
            } else {
                format!(" (the last attempt: {last})")
            };
            return Err(Error::new(format!(
                "GitHub {what} not asked: the release was interrupted by signal {signal}{after}"
            )));
        }
        before_each()?;
        match attempt(client) {
            Ok(value) => return Ok(value),
            Err(error) if authenticated_definitive(&error) => {
                return Err(Error::new(format!("GitHub {what} failed: {error}")));
            }
            Err(error) => {
                last = error.to_string();
                if n < attempts {
                    let pause_for = AUTHENTICATED_BACKOFFS[usize::from(n != 1)];
                    eprintln!(
                        "    GitHub {what} failed (attempt {n}/{attempts}): {last} — retrying in {}s",
                        pause_for.as_secs()
                    );
                    pause(pause_for);
                }
            }
        }
    }
    Err(Error::new(format!(
        "GitHub {what} failed after {}: {last}",
        policy.bound()
    )))
}

/// The retry loop after the caller has selected the operation's authority and policy.
fn retried_with_client<T>(
    client: &Client,
    policy: Policy,
    what: &str,
    mut before_each: impl FnMut() -> Result<()>,
    mut attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    let mut budget = Budget::new(policy);
    let mut n = 1;
    let mut last = String::new();
    loop {
        if let Some(signal) = crate::interrupt::caught() {
            let after = if last.is_empty() {
                String::new()
            } else {
                format!(" (the last attempt: {last})")
            };
            return Err(Error::new(format!(
                "GitHub {what} not asked: the release was interrupted by signal {signal}{after}"
            )));
        }
        before_each()?;
        match attempt(client) {
            Ok(value) => return Ok(value),
            Err(e) => {
                if let Some(signal) = crate::interrupt::caught() {
                    return Err(Error::new(format!(
                        "GitHub {what} not asked: the release was interrupted by signal \
                         {signal} (the last attempt: {e})"
                    )));
                }
                let Some(cause) = retry_cause(&e) else {
                    return Err(Error::new(format!("GitHub {what} failed: {e}")));
                };
                last = e.to_string();
                match budget.after(n, cause) {
                    Decision::Retry(retry) => {
                        eprintln!("    GitHub {what}: {retry}: {last}");
                        pause(retry.pause);
                    }
                    Decision::Exhausted => {
                        return Err(Error::new(format!(
                            "GitHub {what} failed after {}: {last}",
                            budget.bound()
                        )));
                    }
                }
            }
        }
        n = n.saturating_add(1);
    }
}

/// The JSON document at `path` (`repos/{slug}/…`), [`retried`].
/// # Errors
/// As [`retried`]; a 404 among them.
pub(crate) fn get(path: &str) -> Result<Value> {
    get_via(None, path)
}

/// [`get`], its curls started through `spawn` when one is given ([`Launched`]).
/// # Errors
/// As [`get`].
pub(crate) fn get_via(spawn: Option<&Arc<dyn Spawn>>, path: &str) -> Result<Value> {
    let path = api_path(path);
    retried_via(spawn, &format!("GET {path}"), || Ok(()), |c| c.read(&path))
}

/// The JSON document at `path`, or `None` for GitHub's 404 (an answer, never retried).
/// # Errors
/// As [`retried`], but a 404.
pub(crate) fn get_optional(path: &str) -> Result<Option<Value>> {
    let path = api_path(path);
    retried(
        &format!("GET {path}"),
        || Ok(()),
        |c| match c.read(&path) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        },
    )
}

/// Every item of the listing at `base` (`repos/{slug}/releases?per_page=100`), read to its end
/// through `Link` pagination (an incomplete or inconsistent listing is refused), the whole listing
/// [`retried`].
/// # Errors
/// As [`retried`].
pub(crate) fn pages(base: &str, per_page: usize) -> Result<Vec<Value>> {
    pages_via(None, base, per_page)
}

/// [`pages`], its curls started through `spawn` when one is given ([`Launched`]).
/// # Errors
/// As [`pages`].
pub(crate) fn pages_via(
    spawn: Option<&Arc<dyn Spawn>>,
    base: &str,
    per_page: usize,
) -> Result<Vec<Value>> {
    let base = api_path(base);
    retried_via(
        spawn,
        &format!("listing {base}"),
        || Ok(()),
        |c| c.pages(&base, per_page),
    )
}

/// `method` `path` with `json`, a 2xx required, [`retried`] with `before_each` before every
/// attempt: an idempotent mutation only (a metadata PATCH, a guarded DELETE).
/// # Errors
/// As [`retried`].
pub(crate) fn send(
    method: Method,
    path: &str,
    json: Option<&Value>,
    before_each: impl FnMut() -> Result<()>,
) -> Result<Answer> {
    let path = api_path(path);
    retried_via_policy(
        None,
        AUTHENTICATED_RETRY_POLICY,
        &format!("{} {path}", method.name()),
        before_each,
        |c| c.send(method, &path, json).and_then(Answer::success),
    )
}

/// One `method` `path` with `json`, a 2xx required, asked once: the caller re-reads the object to
/// learn what it did.
/// # Errors
/// The client's, and GitHub's refusal.
pub(crate) fn send_once(
    method: Method,
    path: &str,
    json: Option<&Value>,
) -> std::result::Result<Answer, RestError> {
    let client = client().map_err(|e| RestError::Local(e.to_string()))?;
    client
        .send(method, &api_path(path), json)
        .and_then(Answer::success)
}

/// The raw bytes of `path` (a `contents` read, [`ACCEPT_RAW`]), whatever the final status,
/// [`retried`] for a transient answer.
/// # Errors
/// As [`retried`]. A final 4xx remains an [`Answer`] so a caller can distinguish absence.
pub(crate) fn raw(path: &str) -> Result<Answer> {
    let path = api_path(path);
    retried(
        &format!("GET raw {path}"),
        || Ok(()),
        |client| {
            let url = client.api(&path);
            let answer = client
                .exchange(&Request {
                    accept: ACCEPT_RAW,
                    ..Request::get(&url)
                })?
                .answer()?;
            if Cause::from_http(answer.status, answer.retry_after_secs()).is_some() {
                return Err(RestError::Status {
                    status: answer.status,
                    retry_after_secs: answer.retry_after_secs(),
                    message: answer.message(),
                });
            }
            Ok(answer)
        },
    )
}

/// Download asset `id` of `slug` through the shared bounded API/CDN transport.
/// # Errors
/// The client's, the asset API's final answer, the CDN's final answer, or a local file refusal.
pub(crate) fn download_asset(slug: &str, id: u64, dest: &std::path::Path, cap: u64) -> Result<()> {
    let client = client()?;
    client
        .download_asset(&format!("/repos/{slug}/releases/assets/{id}"), dest, cap)
        .map_err(|error| {
            Error::new(format!(
                "download exact GitHub asset-ID {id} on {slug}: {error}"
            ))
        })
}

/// A one-shot POST, built and checked: the caller persists its intent, then [`run_once`]s it.
/// # Errors
/// The client's ([`RestError::Local`] for a URL off GitHub's origins or no token).
pub(crate) fn prepare(url_path: OneShotUrl<'_>, body: Body<'_>) -> Result<Prepared> {
    let client = client()?;
    let url = match url_path {
        OneShotUrl::Api(path) => client.api(&api_path(path)),
        OneShotUrl::Uploads(path) => client.uploads(&api_path(path)),
    };
    client
        .prepare(&Request {
            method: Method::Post,
            url: &url,
            accept: aterm_update_core::github_rest::ACCEPT_JSON,
            body,
            fail_with_body: true,
        })
        .map_err(|e| Error::new(format!("prepare the GitHub POST to {url}: {e}")))
}

/// Where a one-shot POST goes: a path on the API origin, or on the uploads origin.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OneShotUrl<'a> {
    /// `repos/{slug}/releases` on the API origin.
    Api(&'a str),
    /// `repos/{slug}/releases/{id}/assets?name={name}` on the uploads origin.
    Uploads(&'a str),
}

/// Send a [`prepare`]d POST once, as the one-shot protocol reads it: curl's exit as the status,
/// GitHub's answer as stdout, curl's diagnostic as stderr (`--fail-with-body`: an HTTP error is
/// exit 22 and `The requested URL returned error: <code>`).
/// # Errors
/// curl could not be started.
pub(crate) fn run_once(prepared: Prepared) -> Result<RunOut> {
    let exchange = prepared
        .run()
        .map_err(|e| Error::new(format!("one-shot GitHub POST: {e}")))?;
    Ok(RunOut {
        status: exchange.exit.unwrap_or(-1),
        stdout: exchange.body,
        stderr: exchange.stderr.into_bytes(),
    })
}

/// `jq`'s `@tsv` of `fields`: a string with `\`, tab, newline and carriage return escaped; a
/// number in its JSON spelling; a boolean as `true`/`false`; null as nothing.
#[must_use]
pub(crate) fn tsv(fields: &[&Value]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (i, value) in fields.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        match value {
            Value::String(s) => {
                out.reserve(s.len());
                for c in s.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\t' => out.push_str("\\t"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        c => out.push(c),
                    }
                }
            }
            Value::Null => {}
            other => {
                let _ = write!(out, "{other}");
            }
        }
    }
    out
}

/// `value` as `jq`'s `tostring` spells it: a string as itself, anything else as its JSON.
#[must_use]
pub(crate) fn tostring(value: &Value) -> Value {
    match value {
        Value::String(_) => value.clone(),
        other => Value::String(other.to_string()),
    }
}

/// `lines`, each followed by a newline, as `jq` prints one row per output.
#[must_use]
pub(crate) fn rows(lines: impl IntoIterator<Item = String>) -> String {
    lines.into_iter().fold(String::new(), |mut out, line| {
        out.push_str(&line);
        out.push('\n');
        out
    })
}

/// The release-object identity row of `release` as TSV.
#[must_use]
pub(crate) fn identity_row(release: &Value) -> String {
    tsv(&[
        &release["id"],
        &release["tag_name"],
        &tostring(&release["draft"]),
        &release["target_commitish"],
    ])
}

/// The asset rows of `release` as TSV (`name`, `id`, `size`), each with its newline.
#[must_use]
pub(crate) fn asset_rows(release: &Value) -> String {
    rows(
        release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| tsv(&[&a["name"], &tostring(&a["id"]), &tostring(&a["size"])])),
    )
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
