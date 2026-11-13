// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! API reachability measurement and schedule ([`crate::supervise::IdleHost::reach`],
//! [`Route`], [`Route::Custom`], [`route_of`], [`Reach::Unknown`], [`Outcome`], [`Edge`],
//! [`PROBE_BUDGET`], [`NetState`], [`NetState::path`], [`NetState::path_reach`], [`Cadence`],
//! [`NET_LEASE`], [`FRESH`], [`REACH_POLL`], [`PATH_SETTLE`], [`LOST_POLL`], [`LOST_CAP`]).

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use crate::supervise::policy::turn_end::Reach;

/// The host the default route reaches.
pub const API_HOST: &str = "api.anthropic.com";
/// Its port.
pub const API_PORT: u16 = 443;
/// One probe's whole budget: resolve, connect and handshake together.
pub const PROBE_BUDGET: Duration = Duration::from_secs(10);
/// How long one loop's ask keeps the probe running.
pub const NET_LEASE: Duration = Duration::from_secs(60);
/// How old a measure may be, on either clock, and still say anything.
pub const FRESH: Duration = Duration::from_secs(120);
/// The pause after the first, second, and every later miss in a row: a measure that did not say
/// Up (a Down or an Unknown, an overrun among them) and did not end a gap. An upstream DNS or ISP
/// failure leaves this Mac's own network path satisfied, so only this cadence finds the API's
/// return. From the third rung on it costs one probe every 15 s, and only while a lease lives
/// ([`NET_LEASE`]).
pub const DOWN_CADENCE: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(15),
];
/// The pause after an Up.
pub const STEADY_CADENCE: Duration = Duration::from_secs(60);
/// The pause after a measure that ended a gap and did not say Up: the first measure with no fresh
/// one before it, at a loop's first ask or once a sleep made the last one stale. A network coming
/// back from a wake reassociates within seconds of it, so one more probe follows soon, and the
/// [`DOWN_CADENCE`] ladder starts after that one.
pub const GAP_CADENCE: Duration = Duration::from_secs(3);
/// How often a wait on the reach looks again while a measure may land any moment
/// ([`NetState::landing`]): a probe in flight, or one due that has not begun.
pub const REACH_POLL: Duration = Duration::from_secs(1);
/// How long a network path that came back is left to settle (its address, route and resolver)
/// before the probe its return owes ([`NetState::path`]).
pub const PATH_SETTLE: Duration = Duration::from_secs(2);
/// How often a wait on the reach looks again while the path is lost, whether it reads the path
/// alone ([`NetState::path_reach`], [`NetState::path_landing`]) or the probe's measures
/// ([`NetState::landing`]): the path's return is a reading that may come any moment, not a
/// measure on a schedule, so the wait polls for it on the [`DOWN_CADENCE`]'s first rung, as often
/// as a measured miss is first probed again.
pub const LOST_POLL: Duration = DOWN_CADENCE[0];
/// How long a loss of the path holds a session the probe does not measure
/// ([`NetState::path_reach`]), on either clock: past it the path alone reads Unknown, and such a
/// session's wait goes back to its blind ladder, so a path reading that never turns back (an
/// update the monitor missed across a sleep, a path that stays unusable while another route
/// carries traffic) holds no session for ever.
pub const LOST_CAP: Duration = Duration::from_secs(60 * 60);
/// A wall clock that steps back this little (a time sync) is not a sleep.
const CLOCK_SLACK: Duration = Duration::from_secs(5);

/// The pauses a [`NetState`]'s schedule keeps ([`NetState::with_cadence`]); the
/// [`Default`] is the shipped one, which a test may stretch or shorten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// After an Up ([`STEADY_CADENCE`]).
    pub steady: Duration,
    /// After a miss that ended a gap, and the least time from a measure to the probe a path's
    /// return owes ([`GAP_CADENCE`]).
    pub gap: Duration,
    /// From a path's return to the probe it owes ([`PATH_SETTLE`]).
    pub settle: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            steady: STEADY_CADENCE,
            gap: GAP_CADENCE,
            settle: PATH_SETTLE,
        }
    }
}

/// Whether a fact taken at `at` (`at_wall` on the wall clock) is younger than [`FRESH`] on both
/// clocks ([`younger_than`]).
fn young(at: Instant, at_wall: SystemTime, now: Instant, wall: SystemTime) -> bool {
    younger_than(at, at_wall, now, wall, FRESH)
}

/// Whether a fact taken at `at` (`at_wall` on the wall clock) is younger than `limit` on both
/// clocks: the monotonic clock stops while the Mac sleeps and the wall clock does not, and a wall
/// clock that stepped back at most [`CLOCK_SLACK`] (a time sync) counts as no time.
fn younger_than(
    at: Instant,
    at_wall: SystemTime,
    now: Instant,
    wall: SystemTime,
    limit: Duration,
) -> bool {
    let on_wall = match wall.duration_since(at_wall) {
        Ok(d) => d,
        Err(back) if back.duration() <= CLOCK_SLACK => Duration::ZERO,
        Err(_) => return false,
    };
    now.saturating_duration_since(at) < limit && on_wall < limit
}

/// The variables that give an agent a route the host cannot reproduce, every one of them in a
/// shipped binary; in a build that reads development seams, `ANTHROPIC_BASE_URL` holding exactly
/// the lab stub's base URL that [`LAB_ROUTE_SEAM`] names is none ([`lab_route`]).
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
    /// Anything else, or a source the host could not read — the reason, for the log.
    Custom(String),
}

/// One settings source as the host read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Not there: it sets nothing.
    Missing,
    /// There, and not readable (or not a file this reader takes), or where it is cannot be told.
    Unreadable,
    /// Its text.
    Text(String),
}

impl From<Result<Option<String>, String>> for Source {
    /// A document as the settings chain's reader returned it (`claude_settings::Source::text`).
    fn from(text: Result<Option<String>, String>) -> Self {
        match text {
            Ok(None) => Self::Missing,
            Ok(Some(text)) => Self::Text(text),
            Err(_) => Self::Unreadable,
        }
    }
}

/// What decides an agent's route, gathered by the host (aterm-gui `harness_netprobe`, read-only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteFacts {
    /// The agent's exec environment.
    pub env: Option<Vec<String>>,
    /// Every settings source, each named for the log.
    pub settings: Vec<(String, Source)>,
}

/// THE DEVELOPMENT SEAM that lets the lab's Claude Code on its local stub count as on the
/// default route (`lab claude-rotation-proof`, which proves account rotation on the real Claude
/// Code): its value is the stub's exact base URL, `http://<host>:<port>` with a loopback host
/// (`127.0.0.1`, `localhost` or `[::1]`), an explicit port and no path. Read through
/// [`aterm_types::dev_seam!`] once a process ([`lab_route`]): a shipped binary never reads it,
/// so there every [`ROUTE_KEYS`] value names a route.
pub const LAB_ROUTE_SEAM: &str = "ATERM_DEBUG_LAB_DEFAULT_ROUTE";

/// The base URL [`LAB_ROUTE_SEAM`] names in this process, read once: `None` in a shipped binary,
/// and where it is unset or not UTF-8.
fn lab_route_seam() -> Option<&'static str> {
    static SEAM: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SEAM.get_or_init(|| {
        aterm_types::dev_seam!(LAB_ROUTE_SEAM).and_then(|seam| seam.into_string().ok())
    })
    .as_deref()
}

/// Whether `url` is a loopback base URL [`LAB_ROUTE_SEAM`] may name: `http://`, then
/// `127.0.0.1`, `localhost` or `[::1]`, then `:` and a port of 1–65535 in digits, and nothing
/// after it.
fn loopback_base(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some(port) = ["127.0.0.1:", "localhost:", "[::1]:"]
        .iter()
        .find_map(|host| rest.strip_prefix(host))
    else {
        return false;
    };
    !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

/// THE PURE HALF OF [`lab_route`]: whether `key = value` is the lab stub's route that `seam`
/// ([`LAB_ROUTE_SEAM`]'s value, `None` where it is not read) admits as the default route:
/// `key` is `ANTHROPIC_BASE_URL`, `seam` is a loopback base URL ([`loopback_base`]) and the
/// trimmed `value` is exactly `seam`. Any other key, value or seam admits nothing, and `None`
/// (every shipped binary) never does.
#[must_use]
pub fn lab_default_route(seam: Option<&str>, key: &str, value: &str) -> bool {
    key == "ANTHROPIC_BASE_URL"
        && seam.is_some_and(|seam| loopback_base(seam) && value.trim() == seam)
}

/// Whether `key = value` is the lab stub's route this process's [`LAB_ROUTE_SEAM`] admits as the
/// default route ([`lab_default_route`]): never in a shipped binary.
#[must_use]
pub fn lab_route(key: &str, value: &str) -> bool {
    lab_default_route(lab_route_seam(), key, value)
}

/// Whether `key = value` names a route the host cannot reproduce: a [`ROUTE_KEYS`] or
/// [`ROUTE_PREFIXES`] variable with a non-blank value, unless it is the lab stub's route
/// [`LAB_ROUTE_SEAM`] admits ([`lab_route`]; never in a shipped binary).
#[must_use]
pub fn names_a_route(key: &str, value: &str) -> bool {
    !value.trim().is_empty()
        && (ROUTE_KEYS.contains(&key) || ROUTE_PREFIXES.iter().any(|p| key.starts_with(p)))
        && !lab_route(key, value)
}

/// Classify the route from env and settings ([`ROUTE_KEYS`], [`Route::Custom`]).
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
    /// Text that is not a JSON object, or an `env` that is not one.
    Unparsed,
}

/// Parsed settings file ([`settings_of`], [`settings_route`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// Its `env` block, by key.
    pub env: Vec<(String, String)>,
    /// It names an `apiKeyHelper`: any value but `null`, `false` and `""`.
    pub api_key_helper: bool,
    /// It sets a `statusLine`: any value but `null`.
    pub status_line: bool,
    /// Its `statusLine`'s command, where that statusLine is exactly one aterm's tap can carry
    /// (`super::tap::settings_line`): `type` `"command"`, a non-empty `command` string, and at
    /// most a numeric `padding` beside them.
    pub status_command: Option<String>,
    /// It sets `disableAllHooks` to `true`, which also drops a status line.
    pub hooks_off: bool,
}

/// Read one settings file's text.
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
    let status_command = object
        .get("statusLine")
        .and_then(Value::as_object)
        .filter(|line| {
            line.get("type").and_then(Value::as_str) == Some("command")
                && line.iter().all(|(key, value)| match key.as_str() {
                    "type" | "command" => true,
                    "padding" => matches!(value, Value::Number(_)),
                    _ => false,
                })
        })
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned);
    let hooks_off = object.get("disableAllHooks") == Some(&Value::Bool(true));
    Some(Settings {
        env,
        api_key_helper,
        status_line,
        status_command,
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

/// WHY A LAUNCH'S ROUTE VARIABLES NAME NO ONE ROUTE ([`launch_env`]), and whether that is
/// settled for the agent's process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unnamed {
    /// Something was not read this time (the agent's environment, or a settings source that could
    /// not be read or parsed, as one mid-write cannot): read again at the next ask.
    Unread(String),
    /// The launch's sources disagree (one variable set to two values): settled for the process.
    Conflict(String),
}

impl Unnamed {
    /// The reason, for the log.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unnamed::Unread(why) | Unnamed::Conflict(why) => why,
        }
    }

    /// Whether the reason stands for the agent's process ([`Unnamed::Conflict`]), so its gap is
    /// kept; one not read is read again.
    #[must_use]
    pub const fn settled(&self) -> bool {
        matches!(self, Unnamed::Conflict(_))
    }
}

/// THE ROUTE VARIABLES A CLAUDE CODE LAUNCH SETS (v6 §14.17.5.4, the endpoint of a route of its
/// own): the [`ROUTE_KEYS`], [`ROUTE_PREFIXES`] and `NO_PROXY` variables of its exec environment
/// and of every settings source's `env` block, as one `KEY=value` list, a variable blank in all of
/// them left out. Never a precedence guessed: one of them two sources set to different values
/// ([`Unnamed::Conflict`]), or a source that could not be read or parsed, or an environment that
/// could not be read ([`Unnamed::Unread`]), is the reason the launch names no one route.
///
/// # Errors
/// The reason the launch's variables cannot be told, for the log, and whether it is settled.
pub fn launch_env(facts: &RouteFacts) -> Result<Vec<String>, Unnamed> {
    let Some(env) = &facts.env else {
        return Err(Unnamed::Unread(
            "the agent's environment could not be read".to_string(),
        ));
    };
    let mut set: Vec<(String, String, String)> = Vec::new();
    let mut take = |key: &str, value: &str, at: &str| -> Result<(), Unnamed> {
        let value = value.trim();
        let routes = ROUTE_KEYS.contains(&key)
            || ROUTE_PREFIXES.iter().any(|p| key.starts_with(p))
            || matches!(key, "NO_PROXY" | "no_proxy");
        if value.is_empty() || !routes {
            return Ok(());
        }
        match set.iter().find(|(k, _, _)| k == key) {
            Some((_, v, _)) if v == value => Ok(()),
            Some((_, _, first)) => Err(Unnamed::Conflict(format!(
                "{key} is set differently in {first} and {at}"
            ))),
            None => {
                set.push((key.to_string(), value.to_string(), at.to_string()));
                Ok(())
            }
        }
    };
    for (key, value) in env.iter().filter_map(|kv| kv.split_once('=')) {
        take(key, value, "the agent's environment")?;
    }
    for (at, source) in &facts.settings {
        match source {
            Source::Missing => {}
            Source::Unreadable => return Err(Unnamed::Unread(format!("{at} could not be read"))),
            Source::Text(text) => {
                let settings = settings_of(text)
                    .ok_or_else(|| Unnamed::Unread(format!("{at} could not be parsed")))?;
                for (key, value) in &settings.env {
                    take(key, value, at)?;
                }
            }
        }
    }
    Ok(set
        .into_iter()
        .map(|(k, v, _)| format!("{k}={v}"))
        .collect())
}

/// What one probe concluded ([`NetState::finish`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A handshake to the API completed and the platform verified it.
    Up,
    /// A definite failure.
    Down,
    /// Anything else: a timeout, a TLS or certificate failure, another error.
    Unknown,
}

impl Outcome {
    /// The outcome as a log line names it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }
}

/// A probe that changed the verdict ([`NetState::finish`]), which the host logs once. With no
/// path reading the verdict is the last fresh measure's, so this is a probe whose outcome that
/// measure did not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// What the verdict said before this probe: the last fresh measure's outcome, or Down while
    /// a path loss decided it ([`NetState::path`]); `None` when neither said anything (the first
    /// measure after a gap).
    pub from: Option<Outcome>,
    /// What the verdict says after it: this probe's outcome (an overrun read as
    /// [`Outcome::Unknown`]), or Down while a path loss still decides it.
    pub to: Outcome,
    /// How long `from` had held: from the start of its run (its first measure, or the path's
    /// loss) to this probe (zero when `from` is `None`).
    pub held: Duration,
    /// How long this probe took, from its [`NetState::begin`] to its finish (zero for a finish
    /// with no probe in flight).
    pub took: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measure {
    outcome: Outcome,
    /// When it was taken, on both clocks.
    at: Instant,
    at_wall: SystemTime,
    /// When the verdict's run it continues began: the first of the measures in a row that say
    /// it, or the path's loss whose Down it carried on.
    since: Instant,
    /// No fresh measure stood before it, or it was the probe a path's return owed
    /// ([`GAP_CADENCE`]).
    after_gap: bool,
}

/// The network path as the host's watch last read it ([`NetState::path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathFact {
    /// Whether it can carry traffic.
    satisfied: bool,
    /// When it began to read so, on both clocks.
    at: Instant,
    at_wall: SystemTime,
}

/// The verdict as the measures and the path give it, before a flight's overrun
/// ([`NetState::standing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    outcome: Outcome,
    /// When the run began.
    since: Instant,
    /// A path loss decides it.
    by_path: bool,
}

/// The one instance's measure and its schedule ([`Self::next_probe`]), shared by every session
/// that asks.
#[derive(Debug)]
pub struct NetState {
    /// One probe's budget ([`PROBE_BUDGET`]; a test shortens it).
    budget: Duration,
    /// The schedule's pauses ([`Cadence`]; a test stretches or shortens them).
    cadence: Cadence,
    /// Each asking loop's last ask (its session), while its lease lives.
    waiters: HashMap<String, Instant>,
    last: Option<Measure>,
    /// When the probe in flight began (one at a time).
    flight: Option<Instant>,
    /// Misses in a row, the one that ended a gap not among them (the [`DOWN_CADENCE`] rung).
    misses: u32,
    /// The path's last reading ([`Self::path`]), `None` before the first.
    path: Option<PathFact>,
    /// When the path was last lost: its latest change to a reading that cannot carry traffic.
    lost: Option<Instant>,
    /// When the probe a path's return owes is due ([`Self::path`]), until a probe that began then
    /// or later finishes ([`Self::finish`]).
    nudge: Option<Instant>,
}

impl Default for NetState {
    fn default() -> Self {
        Self::with_budget(PROBE_BUDGET)
    }
}

impl NetState {
    /// A state whose probes have `budget` each, on the shipped [`Cadence`].
    #[must_use]
    pub fn with_budget(budget: Duration) -> Self {
        Self::with_cadence(budget, Cadence::default())
    }

    /// A state whose probes have `budget` each and whose schedule keeps `cadence`.
    #[must_use]
    pub fn with_cadence(budget: Duration, cadence: Cadence) -> Self {
        Self {
            budget,
            cadence,
            waiters: HashMap::new(),
            last: None,
            flight: None,
            misses: 0,
            path: None,
            lost: None,
            nudge: None,
        }
    }

    /// One probe's budget.
    #[must_use]
    pub const fn budget(&self) -> Duration {
        self.budget
    }

    /// A loop asks for `who`: its lease renewed, and the verdict now ([`Self::reach`]).
    pub fn ask(&mut self, who: &str, now: Instant, wall: SystemTime) -> Reach {
        self.waiting(now);
        self.waiters.insert(who.to_string(), now);
        self.reach(now, wall)
    }

    /// Current reachability verdict ([`Self::standing`]): Down while a path that is lost now
    /// decides it; else [`Reach::Unknown`] for a probe in flight past its budget, which outweighs
    /// a returned path's standing loss too, so a probe stuck from before the return (a resolve
    /// has no timeout of its own) holds no session at Down past its budget; else the verdict the
    /// measures and the path give (a returned path's standing loss, or the last measure while
    /// fresh); else [`Reach::Unknown`]. A path that can carry traffic never reads Up; only a
    /// probe does.
    #[must_use]
    pub fn reach(&self, now: Instant, wall: SystemTime) -> Reach {
        let run = self.standing(now, wall);
        let lost_now = self.path.is_some_and(|path| !path.satisfied);
        if !(lost_now && run.is_some_and(|run| run.by_path))
            && self
                .flight
                .is_some_and(|began| now.saturating_duration_since(began) > self.budget)
        {
            return Reach::Unknown;
        }
        match run {
            Some(Run {
                outcome: Outcome::Up,
                since,
                ..
            }) => Reach::Up { since },
            Some(Run {
                outcome: Outcome::Down,
                since,
                ..
            }) => Reach::Down { since },
            _ => Reach::Unknown,
        }
    }

    /// Last measure while younger than [`FRESH`] on both clocks.
    fn fresh(&self, now: Instant, wall: SystemTime) -> Option<&Measure> {
        self.last
            .as_ref()
            .filter(|m| young(m.at, m.at_wall, now, wall))
    }

    /// The verdict as the measures and the path give it now, before a flight's overrun: a path
    /// loss that still decides it ([`Self::loss_standing`]) reads Down, from the loss or from
    /// the earlier Down run it continues; otherwise the last measure while fresh; `None` when
    /// neither says anything.
    fn standing(&self, now: Instant, wall: SystemTime) -> Option<Run> {
        let m = self.fresh(now, wall);
        let Some(lost) = self.loss_standing(m, now, wall) else {
            return m.map(|m| Run {
                outcome: m.outcome,
                since: m.since,
                by_path: false,
            });
        };
        Some(Run {
            outcome: Outcome::Down,
            since: m
                .filter(|m| m.outcome == Outcome::Down)
                .map_or(lost, |m| m.since),
            by_path: true,
        })
    }

    /// When the path's latest loss began, while it still decides the verdict: the path is lost
    /// now and no Up was measured since the loss, or it came back less than [`FRESH`] ago (both
    /// clocks) and nothing was measured since its return. `m` is the last fresh measure.
    fn loss_standing(
        &self,
        m: Option<&Measure>,
        now: Instant,
        wall: SystemTime,
    ) -> Option<Instant> {
        let path = self.path?;
        let lost = self.lost?;
        if !path.satisfied {
            return (!m.is_some_and(|m| m.outcome == Outcome::Up && m.at > lost)).then_some(lost);
        }
        (!m.is_some_and(|m| m.at > path.at) && young(path.at, path.at_wall, now, wall))
            .then_some(lost)
    }

    /// The host's watch read the network path now: whether it can carry traffic. Only a change
    /// counts. A loss reads Down ([`Self::reach`]) and drops the probe a return owed and that
    /// has not begun. A return (a change to a path that can carry traffic after a reading that
    /// could not; the watch's first reading is none) owes one probe, due once the path has
    /// settled ([`Cadence::settle`]) and no sooner than [`Cadence::gap`] after the last measure,
    /// which [`Self::next_probe`] brings its due time forward to; a later return moves it.
    /// Whether a probe is now owed, which the probe thread must be woken to see.
    pub fn path(&mut self, satisfied: bool, now: Instant, wall: SystemTime) -> bool {
        let first = match self.path {
            Some(p) if p.satisfied == satisfied => return false,
            Some(_) => false,
            None => true,
        };
        self.path = Some(PathFact {
            satisfied,
            at: now,
            at_wall: wall,
        });
        if !satisfied {
            self.lost = Some(now);
            self.nudge = None;
            return false;
        }
        if first {
            return false;
        }
        self.nudge = Some(self.owed_at(now));
        true
    }

    /// The host heard this Mac's routes move (an address or a link changed) without a change of
    /// whether the path can carry traffic, which is all [`Self::path`] counts: a switch between
    /// two usable paths (a broken Wi-Fi to a hotspot or Ethernet) may bring the API back, and a
    /// host with no path monitor hears only this. It owes one probe as a path's return does, due
    /// once the routes have settled ([`Cadence::settle`]) and no sooner than [`Cadence::gap`]
    /// after the last measure, a later move moving it; none while the path reads lost (its
    /// return owes the probe) or while no lease lives (no probe runs). It changes neither the
    /// path's reading nor its loss. Whether a probe is now owed, which the probe thread must be
    /// woken to see.
    pub fn moved(&mut self, now: Instant) -> bool {
        if self.path.is_some_and(|p| !p.satisfied) || self.waiting(now) == 0 {
            return false;
        }
        self.nudge = Some(self.owed_at(now));
        true
    }

    /// When a probe owed at `now` is due: once the network has settled, and no sooner than
    /// [`Cadence::gap`] after the last measure.
    fn owed_at(&self, now: Instant) -> Instant {
        let settled = now.checked_add(self.cadence.settle).unwrap_or(now);
        let spaced = self.last.and_then(|m| m.at.checked_add(self.cadence.gap));
        spaced.map_or(settled, |spaced| spaced.max(settled))
    }

    /// Whether the path's last reading said it can carry traffic (`None` before the first).
    #[must_use]
    pub fn path_satisfied(&self) -> Option<bool> {
        self.path.map(|p| p.satisfied)
    }

    /// The verdict the path alone gives a session whose API no probe here measures (another
    /// agent's endpoint, or a Claude Code route of its own): Down from the path's latest loss
    /// while it cannot carry traffic (for at most [`LOST_CAP`] on either clock) and for
    /// [`Cadence::settle`] after its return, so a retry does not meet a path still taking its
    /// address, route and resolver; else [`Reach::Unknown`]. A fresh Up measured after the loss
    /// lifts it too: traffic left this Mac, so the reading that the path is lost is not the
    /// whole story (an update the monitor missed). No other measure counts (a Down of
    /// [`API_HOST`] says nothing of another endpoint), and the path never reads Up: once
    /// settled, its return reads Unknown.
    #[must_use]
    pub fn path_reach(&self, now: Instant, wall: SystemTime) -> Reach {
        let (Some(path), Some(lost)) = (self.path, self.lost) else {
            return Reach::Unknown;
        };
        if self.up_since_loss(lost, now, wall) {
            return Reach::Unknown;
        }
        let settling = path
            .at
            .checked_add(self.cadence.settle)
            .is_some_and(|settled| now < settled);
        let holding = !path.satisfied && younger_than(path.at, path.at_wall, now, wall, LOST_CAP);
        if holding || (path.satisfied && settling) {
            Reach::Down { since: lost }
        } else {
            Reach::Unknown
        }
    }

    /// Whether the last fresh measure is an Up taken after the loss at `lost`.
    fn up_since_loss(&self, lost: Instant, now: Instant, wall: SystemTime) -> bool {
        self.fresh(now, wall)
            .is_some_and(|m| m.outcome == Outcome::Up && m.at > lost)
    }

    /// When the path-only verdict ([`Self::path_reach`]) can next change, so a wait on it looks
    /// again then; always later than `now`: [`LOST_POLL`] from now while the path is lost and
    /// its loss holds (its return is a reading, not a scheduled measure; an Up measured since
    /// the loss reads Unknown until it is stale, so it is polled too), the return's settle end
    /// while that is ahead, else `None` (the verdict is Unknown and only a loss moves it, a loss
    /// past [`LOST_CAP`] among them).
    #[must_use]
    pub fn path_landing(&self, now: Instant, wall: SystemTime) -> Option<Instant> {
        let path = self.path?;
        self.lost?;
        if !path.satisfied {
            return younger_than(path.at, path.at_wall, now, wall, LOST_CAP)
                .then(|| now.checked_add(LOST_POLL))
                .flatten();
        }
        path.at
            .checked_add(self.cadence.settle)
            .filter(|settled| *settled > now)
    }

    /// The live leases, the lapsed ones dropped.
    pub fn waiting(&mut self, now: Instant) -> usize {
        self.waiters
            .retain(|_, asked| now.saturating_duration_since(*asked) < NET_LEASE);
        self.waiters.len()
    }

    /// Next probe deadline ([`GAP_CADENCE`], [`DOWN_CADENCE`], [`STEADY_CADENCE`]), or the
    /// probe a path's return owes ([`Self::path`]) when that is due sooner.
    pub fn next_probe(&mut self, now: Instant, wall: SystemTime) -> Option<Instant> {
        if self.waiting(now) == 0 || self.flight.is_some() {
            return None;
        }
        let due = self.fresh(now, wall).map_or(now, |m| {
            let pause = match m.outcome {
                Outcome::Up => self.cadence.steady,
                Outcome::Down | Outcome::Unknown if m.after_gap => self.cadence.gap,
                Outcome::Down | Outcome::Unknown => {
                    let n = usize::try_from(self.misses.max(1) - 1).unwrap_or(usize::MAX);
                    DOWN_CADENCE[n.min(DOWN_CADENCE.len() - 1)]
                }
            };
            m.at.checked_add(pause).unwrap_or(now)
        });
        Some(self.nudge.map_or(due, |nudge| nudge.min(due)))
    }

    /// When a new measure can next land, so a wait on the reach looks again then
    /// ([`crate::supervise::IdleHost::reach_at`]); always later than `now`.
    /// - A probe in flight: its budget's end (where an overrun reads Unknown, unless a path lost
    ///   now decides the verdict, [`Self::reach`]), or [`REACH_POLL`] from now when that is
    ///   sooner or already past.
    /// - None in flight while a lease lives: the next probe's due time
    ///   ([`Self::next_probe`], the probe a path's return owes among them), or [`REACH_POLL`]
    ///   from now when it is due already; and while the path is lost, no later than
    ///   [`LOST_POLL`] from now. Its return is a reading that may come any moment and owes a
    ///   probe a few seconds on ([`Self::path`]), which a wait that sleeps to the ladder's next
    ///   rung (up to 15 s) would meet only at that rung; the wait that looks again on the poll
    ///   finds the owed probe's due time instead.
    /// - None in flight and no lease: `None`, since no probe will run.
    pub fn landing(&mut self, now: Instant, wall: SystemTime) -> Option<Instant> {
        let poll = now.checked_add(REACH_POLL)?;
        if let Some(began) = self.flight {
            return Some(match began.checked_add(self.budget) {
                Some(end) if end > now => end.min(poll),
                _ => poll,
            });
        }
        let due = self.next_probe(now, wall)?;
        let due = if due > now { due } else { poll };
        let lost_look = self
            .path
            .filter(|path| !path.satisfied)
            .and_then(|_| now.checked_add(LOST_POLL));
        Some(lost_look.map_or(due, |look| due.min(look)))
    }

    /// A probe begins: `false` when one is in flight already (one at a time).
    pub const fn begin(&mut self, now: Instant) -> bool {
        if self.flight.is_some() {
            return false;
        }
        self.flight = Some(now);
        true
    }

    /// A probe ended with `outcome`, read as [`Outcome::Unknown`] when it overran its budget.
    /// The [`Edge`] when the verdict's outcome after it is not the one before it (every first
    /// measure after a gap among them, and none for a probe a standing path loss still
    /// outweighs); `None` when it repeats it. The measure continues the verdict's run when it
    /// says the same, and begins one otherwise. A probe that began at or after the due time of
    /// the one a path's return owed ([`Self::path`]) is that one: a miss of it is followed by
    /// [`Cadence::gap`], as a miss that ended a gap is.
    pub fn finish(&mut self, outcome: Outcome, now: Instant, wall: SystemTime) -> Option<Edge> {
        let began = self.flight.take();
        let took = began.map_or(Duration::ZERO, |began| now.saturating_duration_since(began));
        let overran = took > self.budget;
        let outcome = if overran { Outcome::Unknown } else { outcome };
        let gap = self.fresh(now, wall).is_none();
        let before = self.standing(now, wall);
        let nudged = self.nudge.is_some_and(|due| began.unwrap_or(now) >= due);
        if nudged {
            self.nudge = None;
        }
        let since = match before {
            Some(run) if run.outcome == outcome => run.since,
            _ => now,
        };
        let after_gap = gap || nudged;
        // A miss that ended a gap, or the one probe a path's return owed, is followed by the gap
        // cadence and is not a rung of the ladder; a Down and an Unknown climb it together, and
        // an Up leaves it.
        self.misses = if outcome != Outcome::Up && !after_gap {
            self.misses.saturating_add(1)
        } else {
            0
        };
        self.last = Some(Measure {
            outcome,
            at: now,
            at_wall: wall,
            since,
            after_gap,
        });
        let after = self.standing(now, wall)?;
        match before {
            Some(run) if run.outcome == after.outcome => None,
            Some(run) => Some(Edge {
                from: Some(run.outcome),
                to: after.outcome,
                held: now.saturating_duration_since(run.since),
                took,
            }),
            None => Some(Edge {
                from: None,
                to: after.outcome,
                held: Duration::ZERO,
                took,
            }),
        }
    }
}

#[cfg(test)]
#[path = "netwatch_tests.rs"]
mod tests;
