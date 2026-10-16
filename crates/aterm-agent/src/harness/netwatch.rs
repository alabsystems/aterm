// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The API's reach, pure: what the window's supervisor host may say it measured
//! of a Claude Code agent's route to its API
//! ([`crate::supervise::IdleHost::reach`]), and when it measures. The turn-end
//! policy holds a can't-reach-the-API wall while the API is measured down and
//! continues as soon as it is measured up, so this module says exactly what a
//! measure may claim; the I/O half is aterm-gui's `harness_netprobe`.
//!
//! * [`Route`]: which route the agent's requests take. Only the default,
//!   `api.anthropic.com:443` direct, is measured; a base URL, cloud provider,
//!   socket or proxy named in the agent's environment or any settings `env`
//!   block along the chain Claude Code reads
//!   ([`crate::supervise::policy::claude_settings`]) is [`Route::Custom`],
//!   and so is a source that cannot be read ([`route_of`]). A custom route
//!   reads [`Reach::Unknown`].
//! * [`Outcome`]: `Down` only on a definite failure (the name does not
//!   resolve, or every address is refused or unreachable), `Up` on a completed
//!   verified handshake, and `Unknown` for anything else (timeouts, TLS
//!   failures, a probe past [`PROBE_BUDGET`]), so a slow network never reads
//!   as an outage.
//! * [`NetState`]: the one instance's measure and schedule, shared by every
//!   session on the default route. Each waiting loop holds a lease
//!   ([`NET_LEASE`]); with no fresh measure a probe runs at once, then after a
//!   Down at 15 s, 30 s and every 60 s, otherwise every 60 s, and not at all
//!   without a live lease. A measure older than [`FRESH`] on either clock is no
//!   measure (the monotonic clock stops during sleep), and `since` is the start
//!   of the measure's run, so the loop sees an edge only on a change.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use crate::supervise::policy::turn_end::Reach;

/// The host the default route reaches.
pub const API_HOST: &str = "api.anthropic.com";
/// Its port.
pub const API_PORT: u16 = 443;
/// One probe's whole budget: resolve, connect and handshake together.
pub const PROBE_BUDGET: Duration = Duration::from_secs(10);
/// How long one loop's ask keeps the probe running (a loop asks at every wait
/// step, at most 20 s apart).
pub const NET_LEASE: Duration = Duration::from_secs(60);
/// How old a measure may be, on either clock, and still say anything.
pub const FRESH: Duration = Duration::from_secs(120);
/// The pause after the first, second, and every later Down in a row.
pub const DOWN_CADENCE: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// The pause after an Up or an Unknown.
pub const STEADY_CADENCE: Duration = Duration::from_secs(60);
/// A wall clock that steps back this little (a time sync) is not a sleep.
const CLOCK_SLACK: Duration = Duration::from_secs(5);

/// The variables that give an agent a route the host cannot reproduce (Claude
/// Code 2.1.283's: a base URL, its three cloud providers, a unix socket, and
/// the proxies Node and Bun honour). Any non-empty value counts,
/// `CLAUDE_CODE_USE_BEDROCK=0` included: a route misread as custom costs only
/// the time ladder, one misread as the default a hold.
pub const ROUTE_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_UNIX_SOCKET",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];
/// Every variable under these prefixes counts too (`CLAUDE_CODE_PROXY_…`).
pub const ROUTE_PREFIXES: &[&str] = &["CLAUDE_CODE_PROXY"];

/// The route an agent's API requests take, as far as the host can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `api.anthropic.com:443`, direct: the host reproduces it.
    Default,
    /// Anything else, or a source the host could not read — the reason, for
    /// the log.
    Custom(String),
}

/// One settings source as the host read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Not there: it sets nothing.
    Missing,
    /// There, and not readable (or not a file this reader takes), or where
    /// it is cannot be told.
    Unreadable,
    /// Its text.
    Text(String),
}

impl From<Result<Option<String>, String>> for Source {
    /// A document as the settings chain's reader returned it
    /// (`claude_settings::Source::text`).
    fn from(text: Result<Option<String>, String>) -> Self {
        match text {
            Ok(None) => Self::Missing,
            Ok(Some(text)) => Self::Text(text),
            Err(_) => Self::Unreadable,
        }
    }
}

/// What decides an agent's route, gathered by the host (aterm-gui
/// `harness_netprobe`, read-only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteFacts {
    /// The agent's exec environment, `KEY=value` (atpkg's `process_args`, the
    /// one KERN_PROCARGS2 reader); `None` when it could not be read.
    pub env: Option<Vec<String>>,
    /// Every settings source, each named for the log: the chain Claude
    /// Code reads for the agent (`claude_settings::sources_under`) and its
    /// managed preference domains (`claude_settings::managed_preferences`),
    /// as aterm-gui's `harness_netprobe` gathers them.
    pub settings: Vec<(String, Source)>,
}

/// Whether `key = value` names a route the host cannot reproduce.
#[must_use]
pub fn names_a_route(key: &str, value: &str) -> bool {
    !value.trim().is_empty()
        && (ROUTE_KEYS.contains(&key) || ROUTE_PREFIXES.iter().any(|p| key.starts_with(p)))
}

/// The route: [`Route::Default`] only when the agent's environment could be
/// read and names no route ([`ROUTE_KEYS`]), and no settings source sets one
/// in its `env` block — and every source that is there could be read and
/// parsed. Anything else is [`Route::Custom`], with the first reason.
#[must_use]
pub fn route_of(facts: &RouteFacts) -> Route {
    let Some(env) = &facts.env else {
        return Route::Custom("the agent's environment could not be read".to_string());
    };
    for kv in env {
        if let Some((key, value)) = kv.split_once('=')
            && names_a_route(key, value)
        {
            return Route::Custom(format!("{key} in the agent's environment"));
        }
    }
    for (at, source) in &facts.settings {
        match source {
            Source::Missing => {}
            Source::Unreadable => return Route::Custom(format!("{at} could not be read")),
            Source::Text(text) => match settings_route(text) {
                SettingsEnv::Quiet => {}
                SettingsEnv::Names(key) => return Route::Custom(format!("{key} in {at}")),
                SettingsEnv::Unparsed => {
                    return Route::Custom(format!("{at} could not be parsed"));
                }
            },
        }
    }
    Route::Default
}

/// What a settings file's `env` block says of the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEnv {
    /// No route key (or no `env` block at all).
    Quiet,
    /// This route key, set.
    Names(String),
    /// Text that is not a JSON object, or an `env` that is not one: what
    /// Claude Code would make of it is not this reader's to guess.
    Unparsed,
}

/// One settings file as this reader takes it ([`settings_of`]): only what a
/// host judges an agent by. The route reads its `env` block
/// ([`settings_route`]); the accounts' stored-login judgment reads the block
/// and the helper (`super::accounts::ProcessLogin::with_settings`); the usage
/// tap's gate (`super::tap::gate`) reads those and the status line and hook
/// switch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// Its `env` block, by key: each key with its value as text — a string
    /// as it is, `null` as the empty string, any other JSON value as its
    /// JSON text. Empty with no `env` block.
    pub env: Vec<(String, String)>,
    /// It names an `apiKeyHelper` (Claude Code's settings key for a command
    /// that prints a credential): any value but `null`, `false` and `""`.
    pub api_key_helper: bool,
    /// It sets a `statusLine`: any value but `null`. The usage tap is never
    /// put on the relaunch line of a session whose chain sets one when the
    /// line is typed (`super::tap::gate`).
    pub status_line: bool,
    /// It sets `disableAllHooks` to `true`, which also drops a status line.
    pub hooks_off: bool,
}

/// Read one settings file's text: the one settings reader, over the chain's
/// one parse (`claude_settings::parse`: empty or blank is `{}` and a leading
/// byte-order mark is dropped, as Claude Code reads them). `None` for text that
/// is not a JSON object, or whose `env` is not one.
#[must_use]
pub fn settings_of(text: &str) -> Option<Settings> {
    use aterm_json::Value;
    let value = crate::supervise::policy::claude_settings::parse(text).ok()?;
    let object = value.as_object()?;
    let env = match object.get("env") {
        None => Vec::new(),
        Some(env) => env
            .as_object()?
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                (key.clone(), value)
            })
            .collect(),
    };
    let api_key_helper = object
        .get("apiKeyHelper")
        .is_some_and(|helper| match helper {
            Value::Null | Value::Bool(false) => false,
            Value::String(command) => !command.is_empty(),
            _ => true,
        });
    let status_line = object.get("statusLine").is_some_and(|line| !line.is_null());
    let hooks_off = object.get("disableAllHooks") == Some(&Value::Bool(true));
    Some(Settings {
        env,
        api_key_helper,
        status_line,
        hooks_off,
    })
}

/// What a settings file's `env` block says of the route ([`SettingsEnv`]).
#[must_use]
pub fn settings_route(text: &str) -> SettingsEnv {
    let Some(settings) = settings_of(text) else {
        return SettingsEnv::Unparsed;
    };
    settings
        .env
        .into_iter()
        .find(|(key, value)| names_a_route(key, value))
        .map_or(SettingsEnv::Quiet, |(key, _)| SettingsEnv::Names(key))
}

/// What one probe concluded ([`NetState::finish`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A handshake to the API completed and the platform verified it.
    Up,
    /// A definite failure: the name did not resolve, or every address was
    /// refused or unreachable by the kernel's own word.
    Down,
    /// Anything else: a timeout, a TLS or certificate failure, another error.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measure {
    outcome: Outcome,
    /// When it was taken, on both clocks.
    at: Instant,
    at_wall: SystemTime,
    /// When its run began: the first of the measures in a row that say it.
    since: Instant,
}

/// The one instance's measure and its schedule (module header). Pure: every
/// clock is passed in.
#[derive(Debug)]
pub struct NetState {
    /// One probe's budget ([`PROBE_BUDGET`]; a test shortens it).
    budget: Duration,
    /// Each asking loop's last ask (its session), while its lease lives.
    waiters: HashMap<String, Instant>,
    last: Option<Measure>,
    /// When the probe in flight began (one at a time).
    flight: Option<Instant>,
    /// Downs in a row (the cadence after a Down).
    downs: u32,
}

impl Default for NetState {
    fn default() -> Self {
        Self::with_budget(PROBE_BUDGET)
    }
}

impl NetState {
    /// A state whose probes have `budget` each.
    #[must_use]
    pub fn with_budget(budget: Duration) -> Self {
        Self {
            budget,
            waiters: HashMap::new(),
            last: None,
            flight: None,
            downs: 0,
        }
    }

    /// One probe's budget.
    #[must_use]
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// A loop asks for `who` (its session): its lease renewed, and the
    /// verdict now ([`Self::reach`]).
    pub fn ask(&mut self, who: &str, now: Instant, wall: SystemTime) -> Reach {
        // A resolver can hold the worker indefinitely, so the ask expires
        // lapsed leases too: tab churn cannot pile them up meanwhile.
        self.waiting(now);
        self.waiters.insert(who.to_string(), now);
        self.reach(now, wall)
    }

    /// The verdict: the last measure (`Up`/`Down` with the start of its run)
    /// while it is fresh on both clocks and no probe has overrun its budget,
    /// else [`Reach::Unknown`].
    #[must_use]
    pub fn reach(&self, now: Instant, wall: SystemTime) -> Reach {
        if self
            .flight
            .is_some_and(|began| now.saturating_duration_since(began) > self.budget)
        {
            return Reach::Unknown;
        }
        match self.fresh(now, wall).map(|m| (m.outcome, m.since)) {
            Some((Outcome::Up, since)) => Reach::Up { since },
            Some((Outcome::Down, since)) => Reach::Down { since },
            _ => Reach::Unknown,
        }
    }

    /// The last measure, while it is younger than [`FRESH`] on the
    /// monotonic clock and the wall clock (a wall clock that went back more
    /// than a time sync would is no evidence either way: stale).
    fn fresh(&self, now: Instant, wall: SystemTime) -> Option<&Measure> {
        let m = self.last.as_ref()?;
        let on_wall = match wall.duration_since(m.at_wall) {
            Ok(d) => d,
            Err(back) if back.duration() <= CLOCK_SLACK => Duration::ZERO,
            Err(_) => return None,
        };
        (now.saturating_duration_since(m.at) < FRESH && on_wall < FRESH).then_some(m)
    }

    /// The live leases, the lapsed ones dropped.
    pub fn waiting(&mut self, now: Instant) -> usize {
        self.waiters
            .retain(|_, asked| now.saturating_duration_since(*asked) < NET_LEASE);
        self.waiters.len()
    }

    /// When the next probe is due: `None` while no lease lives (the thread
    /// parks) or one is in flight; else the instant — at once with no fresh
    /// measure, else the cadence after the last ([`DOWN_CADENCE`],
    /// [`STEADY_CADENCE`]). An instant not after `now` is now.
    pub fn next_probe(&mut self, now: Instant, wall: SystemTime) -> Option<Instant> {
        if self.waiting(now) == 0 || self.flight.is_some() {
            return None;
        }
        let Some(m) = self.fresh(now, wall) else {
            return Some(now);
        };
        let pause = match m.outcome {
            Outcome::Down => {
                let n = usize::try_from(self.downs.max(1) - 1).unwrap_or(usize::MAX);
                DOWN_CADENCE[n.min(DOWN_CADENCE.len() - 1)]
            }
            Outcome::Up | Outcome::Unknown => STEADY_CADENCE,
        };
        Some(m.at.checked_add(pause).unwrap_or(now))
    }

    /// A probe begins: `false` when one is in flight already (one at a time).
    pub fn begin(&mut self, now: Instant) -> bool {
        if self.flight.is_some() {
            return false;
        }
        self.flight = Some(now);
        true
    }

    /// A probe ended with `outcome`, read as [`Outcome::Unknown`] when it
    /// overran its budget. A measure that repeats the last fresh one continues
    /// its run (`since` kept); any other starts one.
    pub fn finish(&mut self, outcome: Outcome, now: Instant, wall: SystemTime) {
        let overran = self
            .flight
            .take()
            .is_some_and(|began| now.saturating_duration_since(began) > self.budget);
        let outcome = if overran { Outcome::Unknown } else { outcome };
        let since = match self.fresh(now, wall) {
            Some(prev) if prev.outcome == outcome => prev.since,
            _ => now,
        };
        self.downs = if outcome == Outcome::Down {
            self.downs.saturating_add(1)
        } else {
            0
        };
        self.last = Some(Measure {
            outcome,
            at: now,
            at_wall: wall,
            since,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn t0() -> (Instant, SystemTime) {
        (
            Instant::now() + Duration::from_secs(3600),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
        )
    }

    /// One probe, measured at `t` on both clocks.
    fn measured(st: &mut NetState, outcome: Outcome, t: Instant, w: SystemTime) {
        assert!(st.begin(t));
        st.finish(outcome, t, w);
    }

    /// Asks expire lapsed leases themselves while a stuck probe keeps the
    /// worker from pruning, and start no second probe.
    #[test]
    fn asks_expire_churned_sessions_even_while_the_probe_is_stuck() {
        let (t, w) = t0();
        let mut st = NetState::default();
        let mut old = NetState::default();
        assert!(st.begin(t));
        assert!(old.begin(t));
        for n in 0..128_u32 {
            let elapsed = NET_LEASE * n;
            let at = t + elapsed;
            let wall = w + elapsed;
            assert_eq!(st.ask("live-peer", at, wall), Reach::Unknown);
            let who = format!("churn-{n}");
            assert_eq!(st.ask(&who, at, wall), Reach::Unknown);
            assert_eq!(st.waiters.len(), 2, "old leases retire at their boundary");
            assert!(st.waiters.contains_key("live-peer"));
            assert!(st.waiters.contains_key(&who));
            assert_eq!(st.flight, Some(t), "no second probe is started");

            // Control: a bare insert, with no expiry on the ask, keeps every
            // departed session.
            old.waiters.insert("live-peer".to_string(), at);
            old.waiters.insert(who, at);
            assert_eq!(old.reach(at, wall), Reach::Unknown);
        }
        assert_eq!(old.waiters.len(), 129, "the old path grows with history");

        let at = t + NET_LEASE * 127;
        st.ask("new-peer", at + NET_LEASE - S, w + NET_LEASE * 128 - S);
        assert_eq!(st.waiters.len(), 3, "leases not yet expired stay live");
    }

    /// The schedule: parked while nobody waits, a probe at once for the first
    /// waiter, then the Down and steady cadences, one in flight at a time.
    /// Negative control: a waiter that keeps asking keeps it probing.
    #[test]
    fn the_schedule_probes_at_once_then_15_30_60_while_someone_waits_and_parks_when_nobody_does() {
        let (t, w) = t0();
        let mut st = NetState::default();
        assert_eq!(st.next_probe(t, w), None, "nobody waits: parked");
        assert_eq!(st.ask("s-1", t, w), Reach::Unknown, "no measure yet");
        assert_eq!(st.next_probe(t, w), Some(t), "at once");
        assert!(st.begin(t));
        assert!(!st.begin(t), "one in flight at a time");
        assert_eq!(st.next_probe(t, w), None, "none while one flies");
        st.finish(Outcome::Down, t, w);
        let mut at = t;
        for pause in [15, 30, 60, 60] {
            let due = st.next_probe(at, w).expect("a waiter");
            assert_eq!(due, at + pause * S, "after a Down: {pause} s");
            at = due;
            st.ask("s-1", at, w);
            measured(&mut st, Outcome::Down, at, w);
        }
        measured(&mut st, Outcome::Up, at, w);
        assert_eq!(st.next_probe(at, w), Some(at + 60 * S), "after an Up");
        // The lease lapses: parked.
        assert_eq!(st.next_probe(at + 61 * S, w), None);
        assert_eq!(st.waiting(at + 61 * S), 0);
        // The control: asked within the lease, it stays live.
        st.ask("s-1", at + 50 * S, w);
        assert!(st.next_probe(at + 61 * S, w).is_some());
    }

    /// A measure older than [`FRESH`] on either clock says nothing (the
    /// monotonic clock stops while the Mac sleeps, the wall clock does not),
    /// nor does one whose wall clock stepped far back. Negative control: a time
    /// sync's small step back is no sleep.
    #[test]
    fn a_stale_measure_reads_unknown_on_either_clock() {
        let (t, w) = t0();
        let mut st = NetState::default();
        st.ask("s-1", t, w);
        measured(&mut st, Outcome::Down, t, w);
        assert_eq!(st.reach(t + 60 * S, w + 60 * S), Reach::Down { since: t });
        assert_eq!(st.reach(t + 121 * S, w + 60 * S), Reach::Unknown);
        assert_eq!(
            st.reach(t + 10 * S, w + 3600 * S),
            Reach::Unknown,
            "a sleep: the wall clock moved an hour, the monotonic 10 s"
        );
        assert_eq!(st.reach(t + 10 * S, w - 600 * S), Reach::Unknown);
        assert_eq!(
            st.reach(t + 10 * S, w - 2 * S),
            Reach::Down { since: t },
            "a time sync's step back is no sleep"
        );
        // Stale: probed at once again.
        assert_eq!(st.next_probe(t + 10 * S, w + 3600 * S), Some(t + 10 * S));
    }

    /// An Up measured again keeps its `since`, so the loop sees no edge each
    /// minute; a probe past its budget reads Unknown, in flight and after.
    #[test]
    fn since_is_the_runs_start_and_an_overrun_is_unknown() {
        let (t, w) = t0();
        let mut st = NetState::default();
        st.ask("s-1", t, w);
        measured(&mut st, Outcome::Up, t, w);
        measured(&mut st, Outcome::Up, t + 60 * S, w + 60 * S);
        assert_eq!(st.reach(t + 61 * S, w + 61 * S), Reach::Up { since: t });
        measured(&mut st, Outcome::Down, t + 120 * S, w + 120 * S);
        measured(&mut st, Outcome::Up, t + 135 * S, w + 135 * S);
        assert_eq!(
            st.reach(t + 136 * S, w + 136 * S),
            Reach::Up { since: t + 135 * S }
        );
        // In flight past its budget: Unknown meanwhile, and after.
        let began = t + 200 * S;
        assert!(st.begin(began));
        assert_eq!(
            st.reach(began + 5 * S, w + 205 * S),
            Reach::Up { since: t + 135 * S }
        );
        assert_eq!(st.reach(began + 11 * S, w + 211 * S), Reach::Unknown);
        st.finish(Outcome::Down, began + 30 * S, w + 230 * S);
        assert_eq!(st.reach(began + 30 * S, w + 230 * S), Reach::Unknown);
    }

    fn facts(env: &[&str], settings: &[(&str, Source)]) -> RouteFacts {
        RouteFacts {
            env: Some(env.iter().map(|s| (*s).to_string()).collect()),
            settings: settings
                .iter()
                .map(|(at, s)| ((*at).to_string(), s.clone()))
                .collect(),
        }
    }

    /// Every route key, in the environment or a settings `env` block, and every
    /// unreadable or unparseable source makes the route custom. Negative
    /// controls: an empty value, an unrelated key and no `env` are no route.
    #[test]
    fn the_route_is_the_default_only_without_a_base_url_provider_socket_or_proxy() {
        let quiet = [("user", Source::Text("{\"model\":\"opus\"}".to_string()))];
        assert_eq!(
            route_of(&facts(&["HOME=/u", "NO_PROXY=x"], &quiet)),
            Route::Default
        );
        for key in ROUTE_KEYS
            .iter()
            .chain(&["CLAUDE_CODE_PROXY_RESOLVES_HOSTS"])
        {
            let env = format!("{key}=http://gw.example:8080");
            assert!(
                matches!(route_of(&facts(&[&env], &quiet)), Route::Custom(_)),
                "{key} in the environment"
            );
            let block = Source::Text(format!("{{\"env\":{{\"{key}\":\"1\"}}}}"));
            assert!(
                matches!(route_of(&facts(&[], &[("managed", block)])), Route::Custom(w) if w.contains("managed")),
                "{key} in a settings file"
            );
        }
        assert_eq!(
            route_of(&facts(&["HTTPS_PROXY="], &quiet)),
            Route::Default,
            "an empty value names nothing"
        );
        assert!(matches!(
            route_of(&facts(&[], &[("local", Source::Unreadable)])),
            Route::Custom(_)
        ));
        assert!(matches!(
            route_of(&facts(&[], &[("local", Source::Text("{env".to_string()))])),
            Route::Custom(_)
        ));
        assert!(matches!(
            route_of(&facts(
                &[],
                &[("local", Source::Text("{\"env\":[1]}".to_string()))]
            )),
            Route::Custom(_)
        ));
        assert_eq!(
            route_of(&facts(&[], &[("absent", Source::Missing)])),
            Route::Default
        );
        assert!(matches!(route_of(&RouteFacts::default()), Route::Custom(_)));
    }

    /// The settings reader: the `env` block's values as text and whether an
    /// `apiKeyHelper` is named, with the route read through it. Negative
    /// controls: a `null`, `false` or empty helper names none; an
    /// `apiKeyHelper` key inside `env` is a variable, not the setting; text
    /// that is not an object, or whose `env` is not one, is not read.
    #[test]
    fn one_reader_takes_a_settings_files_env_block_and_its_api_key_helper() {
        let text = r#"{"model":"opus","apiKeyHelper":"/usr/local/bin/key",
            "env":{"B":7,"A":"x","C":null,"D":true,"HTTPS_PROXY":"http://gw.example:8080"}}"#;
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            settings_of(text),
            Some(Settings {
                env: vec![
                    pair("A", "x"),
                    pair("B", "7"),
                    pair("C", ""),
                    pair("D", "true"),
                    pair("HTTPS_PROXY", "http://gw.example:8080"),
                ],
                api_key_helper: true,
                ..Settings::default()
            })
        );
        assert_eq!(
            settings_route(text),
            SettingsEnv::Names("HTTPS_PROXY".to_string())
        );
        assert_eq!(settings_of("{}"), Some(Settings::default()));
        assert_eq!(settings_route("{}"), SettingsEnv::Quiet);
        // Empty or blank is `{}`, and a leading byte-order mark is dropped.
        for empty in ["", " \n\t", "\u{feff}", "\u{feff} \n"] {
            assert_eq!(settings_of(empty), Some(Settings::default()), "{empty:?}");
        }
        assert_eq!(
            settings_route("\u{feff}{\"env\":{\"HTTPS_PROXY\":\"http://gw.example:8080\"}}"),
            SettingsEnv::Names("HTTPS_PROXY".to_string())
        );
        for helper in ["null", "false", "\"\""] {
            let quiet = format!("{{\"apiKeyHelper\":{helper}}}");
            assert_eq!(settings_of(&quiet), Some(Settings::default()), "{helper}");
        }
        for helper in ["\"key.sh\"", "true", "[\"key.sh\"]", "{}", "1"] {
            let named = format!("{{\"apiKeyHelper\":{helper}}}");
            assert!(
                settings_of(&named).is_some_and(|s| s.api_key_helper),
                "{helper}"
            );
        }
        assert_eq!(
            settings_of(r#"{"env":{"apiKeyHelper":"key.sh"}}"#),
            Some(Settings {
                env: vec![pair("apiKeyHelper", "key.sh")],
                api_key_helper: false,
                ..Settings::default()
            })
        );
        for unparsed in [
            "{env",
            "[1]",
            "\"text\"",
            "{\"env\":[1]}",
            "{\"env\":\"x\"}",
        ] {
            assert_eq!(settings_of(unparsed), None, "{unparsed}");
            assert_eq!(
                settings_route(unparsed),
                SettingsEnv::Unparsed,
                "{unparsed}"
            );
        }
    }

    /// The same reader says whether a document sets a `statusLine` (any value
    /// but `null`) and `disableAllHooks` to `true`. Negative controls: a
    /// `null` status line, a `false` or non-boolean hook switch, and those
    /// names inside the `env` block set neither.
    #[test]
    fn one_reader_says_whether_a_document_sets_a_status_line_or_turns_hooks_off() {
        let read = |text: &str| settings_of(text).expect("a settings document");
        for set in [
            r#"{"statusLine":{"type":"command","command":"echo mine"}}"#,
            r#"{"statusLine":"x"}"#,
            r#"{"statusLine":{}}"#,
        ] {
            let s = read(set);
            assert!(s.status_line && !s.hooks_off, "{set}");
        }
        assert!(read(r#"{"disableAllHooks":true}"#).hooks_off);
        for quiet in [
            "{}",
            r#"{"statusLine":null}"#,
            r#"{"disableAllHooks":false}"#,
            r#"{"disableAllHooks":"true"}"#,
            r#"{"env":{"statusLine":"x","disableAllHooks":"true"}}"#,
        ] {
            let s = read(quiet);
            assert!(!s.status_line && !s.hooks_off, "{quiet}");
        }
    }
}
