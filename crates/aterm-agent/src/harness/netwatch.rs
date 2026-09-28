// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE API'S REACH, PURE: what the window's supervisor host may say it
//! MEASURED of a Claude Code agent's route to its API
//! ([`crate::supervise::IdleHost::reach`]), and when it measures. The
//! outage of 2026-09-27 is why: every hosted turn ended on `API Error: Can't
//! reach the API server … (ENOTFOUND)`, and the turn-end policy
//! (`supervise/policy/turn_end.rs`, its module header's Walls) now holds such
//! a wall while the API is measured DOWN and continues an unreachable one
//! as soon as it is measured UP (a certificate or proxy refusal stays on its
//! ladder: this handshake trusts the platform's store, the agent its own). A
//! wrong measure there costs a worker up to the hold, or a try the ladder
//! would have spaced — so this module says exactly what a
//! measure may claim, and the I/O half (aterm-gui `harness_netprobe`: the one
//! thread, the resolve, the connect and the verified handshake) only carries
//! it out. Nothing here does I/O.
//!
//! * [`Route`] — WHICH ROUTE the agent's requests take. The host can
//!   reproduce one: the default, `api.anthropic.com:443`, direct. Anything
//!   else — a base URL, a cloud provider, a unix socket, a proxy — named in
//!   the agent's exec environment or in an `env` block of any settings file
//!   Claude Code reads for it (user, project, local, managed and its
//!   drop-ins, a `--settings` on its command line) is [`Route::Custom`], and
//!   a custom route is never measured: its reach reads [`Reach::Unknown`], the
//!   time ladder. So does a source that cannot be read or parsed — a route
//!   the host cannot SEE is one it cannot reproduce ([`route_of`]).
//! * [`Outcome`] — what one probe may conclude, and only that: `Down` on a
//!   DEFINITE failure (the name does not resolve; every address refused, or
//!   unreachable by the kernel's own word), `Up` on a completed handshake
//!   the platform verified, and `Unknown` for everything else — any timeout,
//!   any TLS or certificate failure (a captive portal, an intercepting proxy,
//!   a clock off after sleep), any other error, and a probe that overran its
//!   budget ([`PROBE_BUDGET`]). A slow network, a starved probe thread or a
//!   black-holed IPv6 address never reads as an outage.
//! * [`NetState`] — the ONE instance's measure and its schedule, shared by
//!   every supervised session on the default route: a LEASE per asking loop
//!   ([`NET_LEASE`]; asking is waiting — a loop asks only while it waits at a
//!   wall the network answers), a probe at once when there is no fresh
//!   measure, then after a Down 15 s, 30 s and every 60 s, after an Up or an
//!   Unknown every 60 s, and nothing at all while no lease is live. A measure
//!   older than [`FRESH`] on EITHER clock is no measure (the monotonic clock
//!   stops while the Mac sleeps; the wall clock does not), and `since` is the
//!   START of the measure's run, so the loop sees an edge only when the
//!   measure changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::supervise::policy::turn_end::Reach;

/// The host the default route reaches.
pub const API_HOST: &str = "api.anthropic.com";
/// Its port.
pub const API_PORT: u16 = 443;
/// One probe's whole budget: resolve, connect and handshake together.
pub const PROBE_BUDGET: Duration = Duration::from_secs(10);
/// How long one loop's ask keeps the probe running (the loop asks at the top
/// of every wait step, 20 s at most apart).
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

/// The variables that give an agent a route the host cannot reproduce
/// (Claude Code 2.1.283's own: a base URL, its three cloud providers, a unix
/// socket, and the proxies Node and Bun honour). Any non-empty value counts —
/// `CLAUDE_CODE_USE_BEDROCK=0` included: a route read wrongly as custom
/// costs only the time ladder, one read wrongly as the default a hold.
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

/// Where Claude Code's managed settings live on macOS (its
/// `managed-settings.json` and the `managed-settings.d/*.json` drop-ins).
pub const MANAGED_DIR: &str = "/Library/Application Support/ClaudeCode";
/// Where a managed preference domain would sit (`com.anthropic.claudecode`,
/// read by the 2.1.282 binary): a plist this reader does not parse, so its
/// presence alone makes the route custom.
pub const MANAGED_PREFS_DIR: &str = "/Library/Managed Preferences";
/// The managed preference domain's file name.
pub const MANAGED_PREFS_FILE: &str = "com.anthropic.claudecode.plist";

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
    /// There, and not readable (or not a file this reader takes).
    Unreadable,
    /// Its text.
    Text(String),
}

/// What decides an agent's route, gathered by the host (aterm-gui
/// `harness_netprobe`, read-only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteFacts {
    /// The agent's exec environment, `KEY=value` (atpkg's `process_args`, the
    /// one KERN_PROCARGS2 reader); `None` when it could not be read.
    pub env: Option<Vec<String>>,
    /// Every settings source, each named for the log.
    pub settings: Vec<(String, Source)>,
}

/// Whether `key = value` names a route the host cannot reproduce.
#[must_use]
pub fn names_a_route(key: &str, value: &str) -> bool {
    !value.trim().is_empty()
        && (ROUTE_KEYS.contains(&key) || ROUTE_PREFIXES.iter().any(|p| key.starts_with(p)))
}

/// THE ROUTE: [`Route::Default`] only when the agent's environment could be
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

/// What a settings file's `env` block says of the route ([`SettingsEnv`]).
#[must_use]
pub fn settings_route(text: &str) -> SettingsEnv {
    let Ok(value) = aterm_json::from_str::<aterm_json::Value>(text) else {
        return SettingsEnv::Unparsed;
    };
    let Some(object) = value.as_object() else {
        return SettingsEnv::Unparsed;
    };
    let Some(env) = object.get("env") else {
        return SettingsEnv::Quiet;
    };
    let Some(env) = env.as_object() else {
        return SettingsEnv::Unparsed;
    };
    for (key, value) in env {
        let value = match value {
            aterm_json::Value::String(s) => s.clone(),
            aterm_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        if names_a_route(key, &value) {
            return SettingsEnv::Names(key.clone());
        }
    }
    SettingsEnv::Quiet
}

/// A `--settings` on the agent's command line: a file, or JSON inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsArg {
    /// A path, as written (relative to the agent's directory when not
    /// absolute).
    File(PathBuf),
    /// The JSON itself.
    Inline(String),
}

/// Every `--settings <file|json>` (or `--settings=<…>`) in `argv`, in order.
#[must_use]
pub fn settings_args(argv: &[String]) -> Vec<SettingsArg> {
    let mut out = Vec::new();
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let value = if word == "--settings" {
            words.next().cloned()
        } else {
            word.strip_prefix("--settings=").map(str::to_string)
        };
        if let Some(value) = value {
            out.push(if value.trim_start().starts_with('{') {
                SettingsArg::Inline(value)
            } else {
                SettingsArg::File(PathBuf::from(value))
            });
        }
    }
    out
}

/// The settings files Claude Code reads for an agent whose Claude directory
/// is `claude_dir` and whose working directory is `cwd` — the user's, the
/// project's and the project's local one, and the managed file under
/// `managed` ([`MANAGED_DIR`]) — each named for the log. A Claude directory
/// the agent's environment does not locate, a working directory not known
/// (`cwd` `None` or relative: the project's two files are then left out
/// here), the managed drop-ins and a `--settings` file are the gatherer's to
/// add (an unreadable source, a directory listing, the agent's argv).
#[must_use]
pub fn settings_files(
    claude_dir: Option<&Path>,
    cwd: Option<&Path>,
    managed: &Path,
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(dir) = claude_dir {
        out.push(("the user's settings".to_string(), dir.join("settings.json")));
    }
    if let Some(cwd) = cwd.filter(|c| c.is_absolute()) {
        let project = cwd.join(".claude");
        out.push((
            "the project's settings".to_string(),
            project.join("settings.json"),
        ));
        out.push((
            "the project's local settings".to_string(),
            project.join("settings.local.json"),
        ));
    }
    out.push((
        "the managed settings".to_string(),
        managed.join("managed-settings.json"),
    ));
    out
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

/// THE ONE INSTANCE'S MEASURE AND ITS SCHEDULE (module header). Pure: every
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
        self.waiters.insert(who.to_string(), now);
        self.reach(now, wall)
    }

    /// THE VERDICT: the last measure while it is fresh on both clocks and no
    /// probe has overrun its budget ([`Self::budget`]) — `Up`/`Down` with the start of its run —
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
    /// monotonic clock AND the wall clock (a wall clock that went back more
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

    /// WHEN THE NEXT PROBE IS DUE: `None` while no lease lives (the thread
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

    /// A probe ended with `outcome` — read as [`Outcome::Unknown`] when it
    /// overran its budget, whatever it found. A measure that says what the
    /// last fresh one said continues its run (`since` kept); any other
    /// starts one.
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

    /// THE SCHEDULE: nothing while nobody waits; a probe at once for the
    /// first waiter; after a Down 15 s, 30 s, then every 60 s; after an Up
    /// every 60 s; one in flight at a time; and parked again once every
    /// lease has lapsed (60 s without an ask). NEGATIVE CONTROL: a waiter
    /// that keeps asking keeps it probing.
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

    /// A measure older than two minutes on EITHER clock says nothing: the
    /// monotonic clock stops while the Mac sleeps, the wall clock does not —
    /// and a wall clock stepped far back is no evidence either. NEGATIVE
    /// CONTROL: a fresh one on both clocks says what it measured.
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

    /// `since` IS THE START OF THE RUN: an Up measured again keeps its
    /// `since` (the loop sees no edge each minute); a Down between starts a
    /// new run; a probe that overran its budget is Unknown whatever it
    /// found, and one still in flight past it reads Unknown meanwhile.
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

    /// THE ROUTE is the default only where nothing names another and every
    /// source could be read: each route key in the agent's environment, each
    /// in a settings `env` block (and under the proxy prefix), an
    /// unreadable or unparseable source and an unreadable environment are
    /// each custom. NEGATIVE CONTROLS: an empty value, an unrelated key,
    /// and a settings file with no `env` are no route.
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

    /// `--settings` names a file or carries JSON, spaced or with `=`; the
    /// settings files follow the agent's Claude directory and working
    /// directory (a relative one is none), and the managed file is always
    /// read.
    #[test]
    fn settings_come_from_the_command_line_the_user_the_project_and_the_manager() {
        let argv: Vec<String> = [
            "claude",
            "--settings",
            "team.json",
            "--model",
            "opus",
            "--settings={\"env\":{}}",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            settings_args(&argv),
            [
                SettingsArg::File(PathBuf::from("team.json")),
                SettingsArg::Inline("{\"env\":{}}".to_string()),
            ]
        );
        assert!(settings_args(&["claude".to_string()]).is_empty());
        let managed = Path::new(MANAGED_DIR);
        let files = settings_files(
            Some(Path::new("/u/.claude")),
            Some(Path::new("/w/p")),
            managed,
        );
        let paths: Vec<PathBuf> = files.into_iter().map(|(_, p)| p).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/u/.claude/settings.json"),
                PathBuf::from("/w/p/.claude/settings.json"),
                PathBuf::from("/w/p/.claude/settings.local.json"),
                Path::new(MANAGED_DIR).join("managed-settings.json"),
            ]
        );
        assert_eq!(
            settings_files(
                Some(Path::new("/u/.claude")),
                Some(Path::new("rel")),
                managed
            )
            .len(),
            2
        );
        assert_eq!(settings_files(None, None, managed).len(), 1);
    }
}
