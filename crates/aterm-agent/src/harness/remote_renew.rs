// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One bounded, unattended local renewal requested through an admitted fleet drive.
//! Request IDs are scoped to their caller. An uncertain start is polled, never
//! interpreted as permission to start another sign-in. Results are volatile:
//! an unknown request after a server restart does not establish completion.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(any(unix, test))]
use aterm_json::Value;
use aterm_json::json;

/// The control verb, available to local owners and admitted native drivers.
pub const VERB: &str = "accounts-renew";
const WITHIN: Duration = Duration::from_secs(15 * 60);
const RETAIN: Duration = Duration::from_secs(30 * 60);
#[cfg(any(unix, test))]
const DIAL_WITHIN: Duration = Duration::from_secs(20);
#[cfg(any(unix, test))]
const POLL: Duration = Duration::from_secs(1);
const MAX_JOBS: usize = 16;
const MAX_OUTPUT: usize = 64 * 1024;
#[cfg(unix)]
const MAX_REPLY: usize = 1024 * 1024;

/// The fixed local command's complete bounded output and exit status.
#[derive(Clone)]
pub struct Output {
    /// Zero means the local due pass completed; its output says what it ran.
    pub code: i32,
    /// Complete standard output, never a tail standing in for a complete report.
    pub stdout: String,
    /// Complete standard error.
    pub stderr: String,
}

/// The only two remote operations: start once, then observe that same request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Start(&'a str),
    Poll(&'a str),
}

impl<'a> Request<'a> {
    /// Parse a single bounded, canonical request ID and operation.
    /// # Errors
    /// The operation is unknown or the request ID is not 32 lowercase hex digits.
    pub fn parse(rest: &'a str) -> Result<Self, String> {
        let rest = rest.trim();
        let (kind, nonce) = rest.split_once('=').ok_or_else(usage)?;
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(usage());
        }
        match kind {
            "start" => Ok(Self::Start(nonce)),
            "poll" => Ok(Self::Poll(nonce)),
            _ => Err(usage()),
        }
    }

    const fn nonce(self) -> &'a str {
        match self {
            Self::Start(nonce) | Self::Poll(nonce) => nonce,
        }
    }
}

fn usage() -> String {
    "accounts-renew start=<32 lowercase hex digits> | poll=<32 lowercase hex digits>".into()
}

#[derive(Clone)]
enum State {
    Working,
    Done(Output),
    Failed(String),
    Unavailable,
}

struct Entry {
    at: Instant,
    state: State,
}

#[derive(Default)]
struct Book {
    jobs: Mutex<BTreeMap<(String, String), Entry>>,
}

impl Book {
    fn start(
        self: &Arc<Self>,
        lifetime: &super::account_work::Lifetime,
        caller: &str,
        nonce: &str,
        run: impl FnOnce(&super::account_work::Scope) -> Result<Option<Output>, String> + Send + 'static,
    ) -> Result<State, String> {
        let key = (caller.to_owned(), nonce.to_owned());
        let now = Instant::now();
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.retain(|_, job| {
            matches!(job.state, State::Working) || now.duration_since(job.at) < RETAIN
        });
        if let Some(job) = jobs.get(&key) {
            return Ok(job.state.clone());
        }
        let Some(scope) = lifetime.enter() else {
            return Ok(State::Unavailable);
        };
        if jobs.values().any(|job| matches!(job.state, State::Working)) {
            return Err("another remote renewal is running".into());
        }
        if jobs.len() >= MAX_JOBS
            && let Some(oldest) = jobs
                .iter()
                .min_by_key(|(_, job)| job.at)
                .map(|(key, _)| key.clone())
        {
            jobs.remove(&oldest);
        }
        jobs.insert(
            key.clone(),
            Entry {
                at: now,
                state: State::Working,
            },
        );
        drop(jobs);
        let book = Arc::clone(self);
        let kept = key.clone();
        let running = scope.clone();
        let started = std::thread::Builder::new()
            .name("fleet-account-renewal".into())
            .spawn(scope.wrap(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if running.cancelled() {
                        return Ok(None);
                    }
                    run(&running)
                }));
                let result = match result {
                    Ok(result) => result,
                    Err(payload) => {
                        if let Err(secondary) =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
                        {
                            std::mem::forget(secondary);
                        }
                        Err("renewal worker panicked".into())
                    }
                };
                let state = if running.cancelled() {
                    State::Unavailable
                } else {
                    match result {
                        Ok(Some(output)) => State::Done(output),
                        Ok(None) => State::Unavailable,
                        Err(why) => State::Failed(super::one_line(&why, 320)),
                    }
                };
                if let Some(job) = book
                    .jobs
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get_mut(&kept)
                {
                    job.at = Instant::now();
                    job.state = state;
                }
            }));
        if let Err(error) = started {
            self.jobs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&key);
            return Err(format!("renewal worker could not start: {error}"));
        }
        Ok(State::Working)
    }

    fn poll(&self, caller: &str, nonce: &str) -> Result<State, String> {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.get(&(caller.to_owned(), nonce.to_owned()))
            .filter(|job| matches!(job.state, State::Working) || job.at.elapsed() < RETAIN)
            .map(|job| job.state.clone())
            .ok_or_else(|| {
                "unknown renewal request; the server may have restarted; completion is unconfirmed"
                    .into()
            })
    }
}

fn encode(nonce: &str, state: State) -> String {
    let value = match state {
        State::Working => json!({"schema":1,"request":nonce,"state":"working"}),
        State::Done(output) => json!({"schema":1,"request":nonce,"state":"done",
            "code":output.code,"stdout":output.stdout,"stderr":output.stderr}),
        State::Failed(error) => json!({"schema":1,"request":nonce,"state":"failed","error":error}),
        State::Unavailable => {
            json!({"schema":1,"request":nonce,"state":"unavailable","error":"account work service is unavailable"})
        }
    };
    format!("OK 1\n{value}\n")
}

/// Execute a fixed unattended renewal in this process's own home and configuration.
fn run(
    program: &Path,
    scope: Option<&super::account_work::Scope>,
) -> Result<Option<Output>, String> {
    // The Windows capture retains its established standalone behavior. App-owned work
    // cannot promise cancellation retirement there and is explicitly unavailable.
    #[cfg(not(unix))]
    if scope.is_some() {
        return Ok(None);
    }
    use aterm_uds::spawnfd::Tether as _;
    let mut command = aterm_uds::spawnfd::command(program);
    command.tether();
    command.args(["harness", "accounts", "renew", "--due", "--here"]);
    #[cfg(unix)]
    let output = aterm_uds::deadline::output_with_input_until_watching(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + WITHIN,
        MAX_OUTPUT,
        || scope.is_some_and(super::account_work::Scope::cancelled),
    )
    .map_err(|error| format!("local renewal did not complete: {error}"))?;
    #[cfg(unix)]
    let Some(output) = output else {
        return Ok(None);
    };
    #[cfg(not(unix))]
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + WITHIN,
        MAX_OUTPUT,
    )
    .map_err(|error| format!("local renewal did not complete: {error}"))?;
    #[cfg(unix)]
    let code = {
        use std::os::unix::process::ExitStatusExt as _;
        output
            .status
            .code()
            .unwrap_or_else(|| 128 + output.status.signal().unwrap_or(1))
    };
    #[cfg(not(unix))]
    let code = output.status.code().unwrap_or(1);
    Ok(Some(Output {
        code,
        stdout: String::from_utf8(output.stdout).map_err(|_| "renewal stdout was not UTF-8")?,
        stderr: String::from_utf8(output.stderr).map_err(|_| "renewal stderr was not UTF-8")?,
    }))
}

/// The caller has already passed the current local-owner or fleet-drive gate.
/// No caller-supplied command, account, path or environment reaches the child.
pub fn serve_scoped(lifetime: &super::account_work::Lifetime, caller: &str, rest: &str) -> String {
    serve_with(lifetime, caller, rest, true)
}

/// Serve a standalone node process independently of any GUI App lifetime.
pub fn serve(caller: &str, rest: &str) -> String {
    serve_with(
        &super::account_work::Lifetime::default(),
        caller,
        rest,
        false,
    )
}

fn serve_with(
    lifetime: &super::account_work::Lifetime,
    caller: &str,
    rest: &str,
    scoped: bool,
) -> String {
    static BOOK: OnceLock<Arc<Book>> = OnceLock::new();
    let answer = (|| {
        let request = Request::parse(rest)?;
        let book = BOOK.get_or_init(|| Arc::new(Book::default()));
        let state = match request {
            Request::Start(nonce) => book.start(lifetime, caller, nonce, move |scope| {
                run(&crate::own_exe()?, scoped.then_some(scope))
            })?,
            Request::Poll(nonce) => book.poll(caller, nonce)?,
        };
        Ok::<_, String>(encode(request.nonce(), state))
    })();
    answer.unwrap_or_else(|error| format!("ERR accounts-renew: {}\n", super::one_line(&error, 400)))
}

#[cfg(any(unix, test))]
fn decode(nonce: &str, text: &str) -> Result<State, String> {
    let value: Value = aterm_json::from_str(text).map_err(|_| "invalid renewal reply")?;
    if value["schema"].as_u64() != Some(1) || value["request"].as_str() != Some(nonce) {
        return Err("renewal reply belongs to another request or protocol".into());
    }
    match value["state"].as_str() {
        Some("working") => Ok(State::Working),
        Some("unavailable") => Ok(State::Unavailable),
        Some("failed") => Err(value["error"]
            .as_str()
            .map(|error| {
                format!(
                    "remote renewal did not complete: {}",
                    super::one_line(error, 320)
                )
            })
            .unwrap_or_else(|| "invalid renewal failure reply".into())),
        Some("done") => {
            let code = value["code"]
                .as_i64()
                .filter(|code| (0..=255).contains(code))
                .ok_or("invalid renewal exit code")?;
            let text = |key: &str| {
                value[key]
                    .as_str()
                    .filter(|text| text.len() <= MAX_OUTPUT)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("invalid renewal {key}"))
            };
            Ok(State::Done(Output {
                code: code as i32,
                stdout: text("stdout")?,
                stderr: text("stderr")?,
            }))
        }
        _ => Err("invalid renewal state".into()),
    }
}

#[cfg(any(unix, test))]
fn request_with(
    nonce: &str,
    within: Duration,
    mut call: impl FnMut(&str, Duration) -> Result<String, String>,
    mut pause: impl FnMut(Duration),
) -> Result<Output, String> {
    let deadline = Instant::now() + within;
    let decode_reply = |reply: &str| {
        decode(nonce, reply)
            .map_err(|error| format!("renewal completion unconfirmed (request {nonce}): {error}"))
    };
    // Even a failed write may have reached the peer. Only a poll can recover its
    // answer; repeating start must never be the client's transport retry.
    let mut state = match call(&format!("{VERB} start={nonce}"), within.min(DIAL_WITHIN)) {
        Ok(reply) => decode_reply(&reply)?,
        Err(_) => State::Working,
    };
    loop {
        match state {
            State::Done(output) => return Ok(output),
            State::Failed(error) => return Err(error),
            State::Unavailable => {
                return Err("remote account work service is unavailable".into());
            }
            State::Working => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "renewal completion unconfirmed (request {nonce}); poll the same request"
            ));
        }
        pause(left.min(POLL));
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "renewal completion unconfirmed (request {nonce}); poll the same request"
            ));
        }
        state = decode_reply(
            &call(&format!("{VERB} poll={nonce}"), left.min(DIAL_WITHIN)).map_err(|error| {
                format!("renewal completion unconfirmed (request {nonce}): {error}")
            })?,
        )?;
    }
}

/// Renew the peer's own due accounts through its authenticated native drive.
/// Each connection remains within the fleet's ordinary connection cap.
/// # Errors
/// The fleet does not admit the request, a reply is malformed or for another
/// request, the remote pass fails, or completion is not observed within the bound.
#[cfg(unix)]
pub fn request(host: &str) -> Result<Output, String> {
    use ring::rand::{SecureRandom as _, SystemRandom};
    let mut random = [0; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "renewal request randomness unavailable")?;
    let nonce = aterm_codec::hex::encode_lower(&random);
    request_with(
        &nonce,
        WITHIN + Duration::from_secs(30),
        |line, within| {
            let link = crate::drive_fleet(
                host,
                &aterm_types::control_verbs::Requester::default(),
                within,
            )
            .map_err(|error| error.to_string())?;
            let mut client = crate::RelayClient::new(link.stream);
            let (header, body) = client
                .request_counted_with_limit(line, MAX_REPLY)
                .map_err(|error| error.to_string())?;
            if header != "OK 1" {
                return Err(super::one_line(&header, 400));
            }
            Ok(body)
        },
        std::thread::sleep,
    )
}

#[cfg(not(unix))]
/// Native fleet renewal is unavailable on this platform.
/// # Errors
/// Always returns an unsupported-platform error.
pub fn request(_host: &str) -> Result<Output, String> {
    Err("native fleet renewal is unavailable on this platform".into())
}

#[cfg(test)]
#[path = "remote_renew_tests.rs"]
mod tests;
