// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A caller-bound Claude OAuth challenge handed to the fleet issuer's interactive browser.
//! The issuer never receives mailbox credentials or a Claude credential file. Its answer is
//! only the state-checked loopback callback, delivered by the original opener to its own CLI.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(any(unix, test))]
use aterm_json::Value;
use aterm_json::json;

/// The control verb, available only on a serving window with an interactive browser.
pub const VERB: &str = "claude-challenge";
#[cfg(unix)]
const WITHIN: Duration = Duration::from_secs(180);
const RETAIN: Duration = Duration::from_secs(30 * 60);
#[cfg(any(unix, test))]
const DIAL_WITHIN: Duration = Duration::from_secs(20);
#[cfg(any(unix, test))]
const POLL: Duration = Duration::from_secs(1);
const MAX_JOBS: usize = 16;
const MAX_URL: usize = 16 * 1024;
#[cfg(all(unix, not(test)))]
const MAX_REPLY: usize = 64 * 1024;

/// The one OAuth flow a request ID names.
#[derive(Clone, PartialEq, Eq)]
pub struct Start {
    nonce: String,
    authorize_url: String,
    email: Option<String>,
}

impl std::fmt::Debug for Start {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Start").finish_non_exhaustive()
    }
}

/// A start or poll operation for one caller-owned request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Start the exact flow once, or observe the identical retained start.
    Start(Start),
    /// Observe this request without starting another browser.
    Poll(String),
}

impl Request {
    /// Parse one bounded wire request.
    ///
    /// # Errors
    /// The nonce, operation, fields, email or pinned Claude OAuth URL is invalid.
    pub fn parse(rest: &str) -> Result<Self, String> {
        let mut words = rest.trim().split_ascii_whitespace();
        let operation = words.next().ok_or_else(usage)?;
        let (kind, nonce) = operation.split_once('=').ok_or_else(usage)?;
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(usage());
        }
        match kind {
            "poll" => {
                if words.next().is_some() {
                    return Err(usage());
                }
                Ok(Self::Poll(nonce.to_owned()))
            }
            "start" => {
                let mut authorize_url = None;
                let mut email = None;
                for word in words {
                    if let Some(value) = word.strip_prefix("url=") {
                        if authorize_url.replace(value.to_owned()).is_some() {
                            return Err(usage());
                        }
                    } else if let Some(value) = word.strip_prefix("email=") {
                        if email.replace(value.to_owned()).is_some() {
                            return Err(usage());
                        }
                    } else {
                        return Err(usage());
                    }
                }
                let authorize_url = authorize_url.ok_or_else(usage)?;
                if authorize_url.len() > MAX_URL
                    || email.as_deref().is_some_and(|email| email.len() > 320)
                {
                    return Err("challenge request is too large".into());
                }
                let authorize_url = super::open_url::percent_decode(&authorize_url);
                let email = email.map(|email| super::open_url::percent_decode(&email));
                super::open_url::MailboxFlow::new(&authorize_url)
                    .map_err(|_| "challenge is not a supported Claude OAuth flow".to_owned())?;
                if email.as_deref().is_some_and(|email| {
                    email.is_empty()
                        || email
                            .chars()
                            .any(|ch| ch.is_control() || ch.is_whitespace())
                        || !email.contains('@')
                }) {
                    return Err("challenge email is invalid".into());
                }
                Ok(Self::Start(Start {
                    nonce: nonce.to_owned(),
                    authorize_url,
                    email,
                }))
            }
            _ => Err(usage()),
        }
    }

    fn nonce(&self) -> &str {
        match self {
            Self::Start(start) => &start.nonce,
            Self::Poll(nonce) => nonce,
        }
    }
}

fn usage() -> String {
    "claude-challenge start=<32 lowercase hex digits> url=<percent-encoded OAuth URL> [email=<percent-encoded address>] | poll=<32 lowercase hex digits>".into()
}

#[derive(Clone)]
enum State {
    Working,
    Done(String),
    Failed(String),
}

struct Entry {
    at: Instant,
    start: Start,
    state: State,
}

#[derive(Default)]
struct Book {
    jobs: Mutex<BTreeMap<(String, String), Entry>>,
}

impl Book {
    fn start(
        self: &Arc<Self>,
        caller: &str,
        start: Start,
        run: impl FnOnce(Start) -> Result<String, String> + Send + 'static,
    ) -> Result<State, String> {
        let key = (caller.to_owned(), start.nonce.clone());
        let now = Instant::now();
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.retain(|_, job| {
            matches!(job.state, State::Working) || now.duration_since(job.at) < RETAIN
        });
        if let Some(job) = jobs.get(&key) {
            if job.start != start {
                return Err("challenge request ID names another OAuth flow".into());
            }
            return Ok(job.state.clone());
        }
        if jobs.values().any(|job| matches!(job.state, State::Working)) {
            return Err("another central challenge is running".into());
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
                start: start.clone(),
                state: State::Working,
            },
        );
        drop(jobs);
        let book = Arc::clone(self);
        let kept = key.clone();
        let started = std::thread::Builder::new()
            .name("fleet-claude-challenge".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(start)));
                let state = match result {
                    Ok(Ok(callback)) => State::Done(callback),
                    Ok(Err(why)) => State::Failed(super::one_line(&why, 320)),
                    Err(_) => State::Failed("challenge worker panicked".into()),
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
            });
        if let Err(error) = started {
            self.jobs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&key);
            return Err(format!("challenge worker could not start: {error}"));
        }
        Ok(State::Working)
    }

    fn poll(&self, caller: &str, nonce: &str) -> Result<State, String> {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.get(&(caller.to_owned(), nonce.to_owned()))
            .filter(|job| matches!(job.state, State::Working) || job.at.elapsed() < RETAIN)
            .map(|job| job.state.clone())
            .ok_or_else(|| {
                "unknown challenge request; the server may have restarted; completion is unconfirmed"
                    .into()
            })
    }
}

fn encode(nonce: &str, state: State) -> String {
    let value = match state {
        State::Working => json!({"schema":1,"request":nonce,"state":"working"}),
        State::Done(callback) => json!({"schema":1,"request":nonce,"state":"done",
            "callback":callback}),
        State::Failed(error) => json!({"schema":1,"request":nonce,"state":"failed","error":error}),
    };
    format!("OK 1\n{value}\n")
}

#[cfg(unix)]
fn run(start: Start) -> Result<String, String> {
    super::challenge_window::authorize(&start.authorize_url, start.email.as_deref(), WITHIN)
}

#[cfg(not(unix))]
fn run(_start: Start) -> Result<String, String> {
    Err("central Claude challenges require a serving window on macOS or Linux".into())
}

/// Serve a challenge on this window. The caller has already passed the local-owner or native
/// fleet-drive gate; node-only service processes never reach this entry.
pub fn serve(caller: &str, rest: &str) -> String {
    static BOOK: OnceLock<Arc<Book>> = OnceLock::new();
    let answer = (|| {
        let request = Request::parse(rest)?;
        let nonce = request.nonce().to_owned();
        let book = BOOK.get_or_init(|| Arc::new(Book::default()));
        let state = match request {
            Request::Start(start) => book.start(caller, start, run)?,
            Request::Poll(nonce) => book.poll(caller, &nonce)?,
        };
        Ok::<_, String>(encode(&nonce, state))
    })();
    answer
        .unwrap_or_else(|error| format!("ERR claude-challenge: {}\n", super::one_line(&error, 400)))
}

#[cfg(any(unix, test))]
fn decode(nonce: &str, text: &str) -> Result<State, String> {
    let value: Value = aterm_json::from_str(text).map_err(|_| "invalid challenge reply")?;
    if value["schema"].as_u64() != Some(1) || value["request"].as_str() != Some(nonce) {
        return Err("challenge reply belongs to another request or protocol".into());
    }
    match value["state"].as_str() {
        Some("working") => Ok(State::Working),
        Some("failed") => Ok(State::Failed(
            value["error"]
                .as_str()
                .map(|error| {
                    format!(
                        "central challenge did not complete: {}",
                        super::one_line(error, 320)
                    )
                })
                .unwrap_or_else(|| "invalid challenge failure reply".into()),
        )),
        Some("done") => {
            let callback = value["callback"]
                .as_str()
                .filter(|callback| !callback.is_empty() && callback.len() <= MAX_URL)
                .ok_or("invalid challenge callback")?;
            Ok(State::Done(callback.to_owned()))
        }
        _ => Err("invalid challenge state".into()),
    }
}

#[cfg(any(unix, test))]
fn request_with(
    start: &Start,
    within: Duration,
    mut call: impl FnMut(&str, Duration) -> Result<String, String>,
    mut pause: impl FnMut(Duration),
) -> Result<String, String> {
    let deadline = Instant::now() + within;
    let decode_reply = |reply: &str| {
        decode(&start.nonce, reply).map_err(|error| {
            format!(
                "challenge completion unconfirmed (request {}): {error}",
                start.nonce
            )
        })
    };
    let wire = format!(
        "{VERB} start={} url={}{}",
        start.nonce,
        super::open_url::percent_encode(&start.authorize_url),
        start.email.as_deref().map_or_else(String::new, |email| {
            format!(" email={}", super::open_url::percent_encode(email))
        })
    );
    let mut state = match call(&wire, within.min(DIAL_WITHIN)) {
        Ok(reply) => decode_reply(&reply)?,
        Err(_) => State::Working,
    };
    loop {
        match state {
            State::Done(callback) => return Ok(callback),
            State::Failed(error) => return Err(error),
            State::Working => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "challenge completion unconfirmed (request {}); poll the same request",
                start.nonce
            ));
        }
        pause(left.min(POLL));
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "challenge completion unconfirmed (request {}); poll the same request",
                start.nonce
            ));
        }
        state = decode_reply(
            &call(
                &format!("{VERB} poll={}", start.nonce),
                left.min(DIAL_WITHIN),
            )
            .map_err(|error| {
                format!(
                    "challenge completion unconfirmed (request {}): {error}",
                    start.nonce
                )
            })?,
        )?;
    }
}

#[cfg(all(unix, not(test)))]
fn request(host: &str, authorize_url: &str, email: Option<&str>) -> Result<String, String> {
    use ring::rand::{SecureRandom as _, SystemRandom};
    let mut random = [0; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "challenge request randomness unavailable")?;
    let start = Start {
        nonce: aterm_codec::hex::encode_lower(&random),
        authorize_url: authorize_url.to_owned(),
        email: email.map(str::to_owned),
    };
    super::open_url::MailboxFlow::new(authorize_url)?;
    let callback = request_with(
        &start,
        WITHIN + Duration::from_secs(30),
        |line, within| {
            let link = crate::drive_fleet(host, within).map_err(|error| error.to_string())?;
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
    )?;
    let flow = super::open_url::MailboxFlow::new(authorize_url)?;
    if !flow.is_callback(&callback) {
        return Err("central challenge returned another OAuth flow's callback".into());
    }
    Ok(callback)
}

/// Route a blocked Claude OAuth flow to the fleet issuer's serving window.
#[cfg(all(unix, not(test)))]
pub fn request_central(authorize_url: &str, email: Option<&str>) -> Result<String, String> {
    let state = aterm_types::dirs::state_dir().ok_or("no fleet state directory")?;
    let peers = aterm_net::fleet_member::read_peers(&state)
        .map_err(|_| "fleet peers are unavailable".to_owned())?;
    let issuer = peers
        .iter()
        .find(|peer| peer.via == aterm_net::fleet_member::Via::Issuer)
        .ok_or("no fleet issuer is configured")?;
    request(&issuer.host, authorize_url, email)
}

#[cfg(all(unix, test))]
thread_local! {
    static CENTRAL_RESULT: std::cell::RefCell<Option<Result<String, String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Restores the previous inert central result when dropped.
#[cfg(all(unix, test))]
pub struct CentralResultGuard(Option<Result<String, String>>);

#[cfg(all(unix, test))]
impl Drop for CentralResultGuard {
    fn drop(&mut self) {
        CENTRAL_RESULT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// Substitute one thread-local central result; no test reaches the live fleet without this.
#[cfg(all(unix, test))]
pub fn set_central_result_for_tests(result: Result<String, String>) -> CentralResultGuard {
    CentralResultGuard(CENTRAL_RESULT.with(|slot| slot.borrow_mut().replace(result)))
}

/// Return the thread's substituted result, or an inert refusal by default.
#[cfg(all(unix, test))]
pub fn request_central(_authorize_url: &str, _email: Option<&str>) -> Result<String, String> {
    CENTRAL_RESULT.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(|| Err("no central challenge is configured in this test".into()))
    })
}

/// Report that no visible central browser is available on this platform.
#[cfg(not(unix))]
pub fn request_central(_authorize_url: &str, _email: Option<&str>) -> Result<String, String> {
    Err("central Claude challenges are unavailable on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const OTHER: &str = "1123456789abcdef0123456789abcdef";
    const URL: &str = "https://claude.ai/oauth/authorize?client_id=claude&redirect_uri=http%3A%2F%2F127.0.0.1%3A4321%2Fcallback&state=state";
    const CALLBACK: &str = "http://127.0.0.1:4321/callback?code=secret&state=state";

    fn start() -> Start {
        Start {
            nonce: ID.into(),
            authorize_url: URL.into(),
            email: Some("owner@example.test".into()),
        }
    }

    #[test]
    fn only_canonical_bounded_requests_reach_the_worker() {
        let wire = format!(
            "start={ID} url={} email={}",
            super::super::open_url::percent_encode(URL),
            super::super::open_url::percent_encode("owner@example.test")
        );
        assert_eq!(Request::parse(&wire), Ok(Request::Start(start())));
        let shown = format!("{:?}", start());
        assert!(!shown.contains(URL) && !shown.contains("owner@example.test"));
        assert_eq!(
            Request::parse(&format!("poll={ID}")),
            Ok(Request::Poll(ID.into()))
        );
        for rest in [
            String::new(),
            "start=".into(),
            format!("start={ID}"),
            format!("start={ID} url=not-a-flow"),
            format!("poll={ID} trailing"),
            format!("run={ID}"),
            format!("poll={}", ID.to_uppercase()),
            format!("poll={ID}0"),
        ] {
            assert!(Request::parse(&rest).is_err(), "{rest:?}");
        }
    }

    #[test]
    fn duplicates_share_one_flow_and_callers_cannot_read_each_others_results() {
        let book = Arc::new(Book::default());
        let (release, wait) = std::sync::mpsc::channel();
        assert!(matches!(
            book.start("a", start(), move |_| {
                wait.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
                Ok(CALLBACK.into())
            }),
            Ok(State::Working)
        ));
        assert!(matches!(
            book.start("a", start(), |_| panic!("duplicate execution")),
            Ok(State::Working)
        ));
        let mut changed = start();
        changed.authorize_url.push_str("&client_id=other");
        assert!(
            book.start("a", changed, |_| panic!("changed flow"))
                .is_err()
        );
        assert!(book.poll("b", ID).is_err());
        assert!(
            book.start(
                "b",
                Start {
                    nonce: OTHER.into(),
                    ..start()
                },
                |_| { panic!("parallel execution") }
            )
            .is_err()
        );
        release.send(()).unwrap();
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(1),
            || matches!(book.poll("a", ID), Ok(State::Done(_))).then_some(()),
        )
        .expect("the owned worker finished");
        assert!(book.poll("b", ID).is_err());
    }

    #[test]
    fn a_lost_start_reply_is_recovered_without_another_browser() {
        let mut calls = Vec::new();
        let result = request_with(
            &start(),
            Duration::from_secs(60),
            |line, _| {
                calls.push(line.to_owned());
                if calls.len() == 1 {
                    Err("reply lost after acceptance".into())
                } else {
                    Ok(
                        json!({"schema":1,"request":ID,"state":"done","callback":CALLBACK})
                            .to_string(),
                    )
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(result, CALLBACK);
        assert_eq!(
            calls.iter().filter(|line| line.contains("start=")).count(),
            1
        );
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn complete_replies_preserve_callback_secrets_but_failures_never_quote_them() {
        let State::Done(callback) = decode(
            ID,
            &json!({"schema":1,"request":ID,"state":"done","callback":CALLBACK}).to_string(),
        )
        .unwrap() else {
            panic!("complete")
        };
        assert_eq!(callback, CALLBACK);
        let State::Failed(reason) = decode(
            ID,
            &json!({"schema":1,"request":ID,"state":"failed","error":"window unavailable"})
                .to_string(),
        )
        .unwrap() else {
            panic!("failed reply")
        };
        assert!(reason.contains("window unavailable"));
        for change in ["schema", "request", "state", "callback"] {
            let mut bad: Value =
                json!({"schema":1,"request":ID,"state":"done","callback":CALLBACK});
            let value = match change {
                "schema" => json!(2),
                "request" => json!(OTHER),
                "state" => json!("unknown"),
                _ => json!("x".repeat(MAX_URL + 1)),
            };
            bad.as_object_mut().unwrap().insert(change.into(), value);
            assert!(decode(ID, &bad.to_string()).is_err(), "{change}");
        }
    }
}
