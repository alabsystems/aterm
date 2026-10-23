// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub's REST API for the release cutter, routed through [`aterm_update_core::github_rest`].

use std::io::Write as _;
use std::sync::Arc;

use aterm_json::Value;
use aterm_uds::spawnfd::{Child, Command, Stdio};
use aterm_update_core::github_rest::{
    Answer, Body, Client, Method, Origins, Prepared, Request, RestError, Spawn, Token,
};

use crate::ledger::{Error, Result, RunOut};

/// The `Accept` of a raw content read (`/repos/{slug}/contents/{path}`): the file's bytes, not its
/// JSON envelope.
pub(crate) const ACCEPT_RAW: &str = "application/vnd.github.raw";

/// The backoffs between [`retried`]'s attempts, in seconds.
const BACKOFFS: [u64; 2] = [2, 5];

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
        let (mut child, guard) = self.0.spawn(command)?;
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
fn api_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Whether asking again cannot change `e`: GitHub's definitive answer (a 4xx but 408 and 429), a
/// redirect refused, or a refusal made here before curl ran.
#[must_use]
pub(crate) fn definitive(e: &RestError) -> bool {
    match e {
        RestError::Local(_) | RestError::Redirect(_) => true,
        RestError::Status { status, .. } => {
            (400..500).contains(status) && !matches!(status, 408 | 429)
        }
        RestError::Transport(_) | RestError::Malformed(_) => false,
    }
}

/// Wait `seconds` between attempts, ending early once an interrupt is caught (no wait under a
/// test's [`Fixture`]).
fn pause(seconds: u64) {
    #[cfg(test)]
    if FIXTURE.with(|f| f.borrow().is_some()) {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while crate::interrupt::caught().is_none() {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(std::time::Duration::from_millis(50)));
    }
}

/// `attempt`, made up to three times, `before_each` before every one.
/// # Errors
/// `before_each`'s; a definitive failure at once; an interrupt caught before an attempt; the last
/// failure after three attempts.
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
    mut before_each: impl FnMut() -> Result<()>,
    mut attempt: impl FnMut(&Client) -> std::result::Result<T, RestError>,
) -> Result<T> {
    let client = client_via(spawn)?;
    let attempts = BACKOFFS.len() + 1;
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
        match attempt(&client) {
            Ok(value) => return Ok(value),
            Err(e) if definitive(&e) => {
                return Err(Error::new(format!("GitHub {what} failed: {e}")));
            }
            Err(e) => {
                last = e.to_string();
                if let Some(backoff) = BACKOFFS.get(n - 1) {
                    eprintln!(
                        "    GitHub {what} failed (attempt {n}/{attempts}): {last} — retrying in \
                         {backoff}s"
                    );
                    pause(*backoff);
                }
            }
        }
    }
    Err(Error::new(format!(
        "GitHub {what} failed after {attempts} attempts: {last}"
    )))
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
    retried(&format!("{} {path}", method.name()), before_each, |c| {
        c.send(method, &path, json).and_then(Answer::success)
    })
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

/// The raw bytes of `path` (a `contents` read, [`ACCEPT_RAW`]), whatever the status, asked once.
/// # Errors
/// The client's, and a transport failure.
pub(crate) fn raw(path: &str) -> std::result::Result<Answer, RestError> {
    let client = client().map_err(|e| RestError::Local(e.to_string()))?;
    let url = client.api(&api_path(path));
    client
        .exchange(&Request {
            accept: ACCEPT_RAW,
            ..Request::get(&url)
        })?
        .answer()
}

/// The bytes of asset `id` of `slug`, streaming: the asset API's redirect, checked against the
/// asset CDN's origins, then an anonymous curl of the CDN with its stdout and stderr piped.
/// # Errors
/// The client's; the asset API's answer when it is not a redirect onto the CDN.
pub(crate) fn open_asset(slug: &str, id: u64) -> std::result::Result<Child, RestError> {
    let client = client().map_err(|e| RestError::Local(e.to_string()))?;
    client.open_asset(&format!("/repos/{slug}/releases/assets/{id}"))
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
    fields
        .iter()
        .map(|value| match value {
            Value::String(s) => {
                let mut out = String::with_capacity(s.len());
                for c in s.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\t' => out.push_str("\\t"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        c => out.push(c),
                    }
                }
                out
            }
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\t")
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
