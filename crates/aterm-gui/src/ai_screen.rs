// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AI CLI status projection and login actions.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use aterm_agent::ai::config::{FleetState, From, Settings};
use aterm_agent::ai::connector;
use aterm_agent::ai::fleet::Held;
use aterm_agent::ai::permission::Choice;
use aterm_agent::ai::state::{Mark, State};
use aterm_messages::Severity;
use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Mover, Reason, Resurface, Then, Why};

use crate::App;
use crate::native_settings::SettingsRoute;

/// How often the window looks with nothing else asking.
pub(crate) const LOOK_EVERY: Duration = Duration::from_hours(6);

const WAKE_POLL: Duration = Duration::from_secs(5);

const FACT_MAX: usize = 400;

/// One AI CLI as the window's look judged it: plain data, its words already sanitized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiCli {
    /// The connection's id (`codex`).
    pub(crate) id: &'static str,
    /// The product's name (`Codex CLI`).
    pub(crate) product: &'static str,
    /// Whether this is Claude Code, whose login accounts v2 owns.
    pub(crate) claude: bool,
    /// Whether it is wanted here.
    pub(crate) wanted: bool,
    /// Its state.
    pub(crate) state: State,
    /// How much it matters.
    pub(crate) mark: Mark,
    /// What is known, one line.
    pub(crate) fact: String,
    /// THE PATH `fact` NAMES, as it says it ([`named_path`]).
    pub(crate) path: Option<String>,
    /// The installed version.
    pub(crate) version: Option<String>,
}

fn named_path(fact: &str, foreign: Option<&Path>) -> Option<String> {
    foreign
        .map(|path| one_line(&path.display().to_string()))
        .filter(|path| !path.is_empty() && fact.contains(path.as_str()))
}

impl AiCli {
    /// Whether a definite install or sign-in action is needed.
    #[must_use]
    pub(crate) const fn needs_you(&self) -> bool {
        self.state.needs_person(self.wanted)
    }

    /// The act its row offers where it needs a person.
    #[must_use]
    pub(crate) fn act(&self) -> Option<AiAct> {
        self.needs_you()
            .then_some(if self.state == State::NotInstalled {
                AiAct::Install
            } else {
                AiAct::SignIn
            })
    }
}

/// What the window's look read of every CLI, in `VENDORS` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiReading {
    /// Unix seconds of the look.
    pub(crate) as_of: i64,
    /// The zone's offset then, for the time the page shows.
    pub(crate) offset_s: Option<i64>,
    /// The preferred CLI the rows were judged under.
    pub(crate) preferred: &'static str,
    /// Every CLI.
    pub(crate) clis: Vec<AiCli>,
    /// Why the AI store's record was not used, if it was not.
    pub(crate) store_note: Option<String>,
    /// What aterm holds of a fleet document from a URL, its last error one sanitized line.
    pub(crate) held: Option<Held>,
}

impl AiReading {
    /// The window's reading of `look`, taken at `now`.
    #[must_use]
    pub(crate) fn of(look: &aterm_agent::ai::cli::Look, now: i64, offset_s: Option<i64>) -> Self {
        let clis = connector::connections()
            .filter_map(|c| look.rows.iter().find(|row| row.connector.id == c.id))
            .map(|row| {
                let fact = one_line(&row.fact);
                let foreign = look
                    .facts
                    .iter()
                    .find(|facts| facts.connector.id == row.connector.id)
                    .and_then(|facts| facts.foreign.as_deref());
                AiCli {
                    id: row.connector.id,
                    product: row.connector.product,
                    claude: row.connector.is_claude(),
                    wanted: row.wanted,
                    state: row.state,
                    mark: row.mark,
                    path: named_path(&fact, foreign),
                    fact,
                    version: row.version.as_deref().map(one_line),
                }
            })
            .collect();
        Self {
            as_of: now,
            offset_s,
            preferred: look.settings.preferred.0.id,
            clis,
            store_note: look.store_note.as_deref().map(one_line),
            held: look.held.clone().map(|held| Held {
                error: held.error.as_deref().map(one_line),
                ..held
            }),
        }
    }

    /// The CLI `id`, if the reading has it.
    #[must_use]
    pub(crate) fn cli(&self, id: &str) -> Option<&AiCli> {
        self.clis.iter().find(|cli| cli.id == id)
    }
}

fn one_line(text: &str) -> String {
    aterm_agent::ai::review::sanitize_block(text, FACT_MAX)
        .replace(['\n', '\t'], " ")
        .trim()
        .to_string()
}

/// This machine's AI choices, resolved (`[ai]` over the fleet document).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiChoices {
    /// The preferred CLI and where that came from.
    pub(crate) preferred: (&'static str, From),
    /// Whether reviews run, and where that came from.
    pub(crate) reviews: (bool, From),
    /// The reviewers' permission level, and where that came from.
    pub(crate) review_permissions: (Choice, From),
    /// The reviewers, in order, and where they came from.
    pub(crate) reviewers: (Vec<&'static str>, From),
    /// Reviewers per review.
    pub(crate) review_count: u8,
    /// Each reviewer's time limit, seconds.
    pub(crate) review_timeout_s: u32,
    /// Each CLI's `use` and where it came from, in `VENDORS` order.
    pub(crate) uses: Vec<(&'static str, bool, From)>,
    /// This machine's name in the fleet document.
    pub(crate) machine: Option<String>,
    /// Where the fleet document comes from, as shown.
    pub(crate) fleet_source: Option<String>,
    /// The fleet document's pinned signer.
    pub(crate) fleet_key: Option<String>,
    /// The fleet document's fate.
    pub(crate) fleet: FleetState,
    /// What the readers said, one line each.
    pub(crate) notes: Vec<String>,
}

impl AiChoices {
    fn wants(&self, id: &str) -> bool {
        self.uses.iter().any(|(cli, on, _)| *cli == id && *on)
    }

    /// The choices `settings` resolve.
    #[must_use]
    pub(crate) fn of(settings: &Settings) -> Self {
        Self {
            preferred: (settings.preferred.0.id, settings.preferred.1),
            reviews: settings.reviews,
            review_permissions: settings.review_permissions,
            reviewers: (
                settings.reviewers.0.iter().map(|c| c.id).collect(),
                settings.reviewers.1,
            ),
            review_count: settings.review_count.0,
            review_timeout_s: settings.review_timeout_s.0,
            uses: settings
                .uses
                .iter()
                .map(|(c, on, from)| (c.id, *on, *from))
                .collect(),
            machine: settings.machine.clone(),
            fleet_source: settings.fleet_source.as_deref().map(one_line),
            fleet_key: settings.fleet_key.clone(),
            fleet: match &settings.fleet {
                FleetState::Refused { source, why } => FleetState::Refused {
                    source: one_line(source),
                    why: one_line(why),
                },
                other => other.clone(),
            },
            notes: settings.notes.iter().map(|note| one_line(note)).collect(),
        }
    }
}

impl Default for AiChoices {
    fn default() -> Self {
        Self::of(&aterm_agent::ai::config::resolve(
            &aterm_agent::ai::config::Local::default(),
            None,
            None,
            Vec::new(),
        ))
    }
}

/// Settings ▸ Harness ▸ AI CLIs snapshot ([`Self::projection`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AiState {
    /// Whether the window looks (`[ai] enabled`).
    pub(crate) enabled: bool,
    /// A look is running (Refresh is busy).
    pub(crate) looking: bool,
    /// The choices in force.
    pub(crate) choices: AiChoices,
    /// The newest look's reading.
    pub(crate) reading: Option<AiReading>,
    /// Why no look could be taken (the worker did not start, the look failed).
    pub(crate) unavailable: Option<String>,
}

/// The act a CLI's row offers where it needs a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiAct {
    /// `aterm ai login <cli>`: its vendor's own sign-in, in a new tab.
    SignIn,
    /// `aterm pkg install <cli>`: wanted here and not installed.
    Install,
}

impl AiAct {
    /// The button's word.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::SignIn => "Sign in",
            Self::Install => "Install",
        }
    }
}

/// One row of the CLIS card: what it is, its state, and the act it offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiCliRow {
    /// The CLI's id.
    pub(crate) id: &'static str,
    /// The product's name.
    pub(crate) product: &'static str,
    /// Its name and version, and whether it is the preferred one.
    pub(crate) title: String,
    /// Its state and what is known.
    pub(crate) detail: String,
    /// The path `detail` names ([`AiCli::path`]), which a one-line row keeps the end of.
    pub(crate) path: Option<String>,
    /// Sign in or Install, where it needs a person.
    pub(crate) act: Option<AiAct>,
    /// Whether its `use` comes from the fleet document.
    pub(crate) use_from_fleet: bool,
}

/// What Settings ▸ Harness ▸ AI CLIs renders ([`AiState::projection`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiProjection {
    /// Whether the host has published.
    pub(crate) observed: bool,
    /// Whether the window looks.
    pub(crate) enabled: bool,
    /// A look is running.
    pub(crate) looking: bool,
    /// The CLIS card's first line: who needs you, or that no one does.
    pub(crate) health: String,
    /// One row per CLI, in `VENDORS` order.
    pub(crate) clis: Vec<AiCliRow>,
    /// Whether the preferred CLI comes from the fleet document.
    pub(crate) preferred_from_fleet: bool,
    /// The REVIEWS card's facts.
    pub(crate) review_rows: Vec<(String, String)>,
    /// The FLEET card's facts.
    pub(crate) fleet_rows: Vec<(String, String)>,
    /// Whether `[ai] fleet` names a URL (Fetch now has something to fetch).
    pub(crate) fleet_from_url: bool,
}

const fn from_fleet(from: From) -> bool {
    matches!(from, From::Fleet | From::FleetMachine)
}

const fn from_words(from: From) -> &'static str {
    match from {
        From::Local => "aterm.toml",
        From::FleetMachine => "the fleet document, this machine's row",
        From::Fleet => "the fleet document",
        From::Default => "the default",
    }
}

fn detail_words(word: &str, fact: &str) -> String {
    if fact.starts_with(&word.replace('-', " ")) {
        fact.to_string()
    } else {
        format!("{word} \u{b7} {fact}")
    }
}

fn listed(names: &[&str]) -> String {
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

impl AiState {
    /// An action needs both the current policy and the observed need.
    pub(crate) fn actionable_cli(&self, id: &str) -> Option<&AiCli> {
        self.reading
            .as_ref()
            .filter(|_| self.enabled && self.choices.wants(id))
            .and_then(|reading| reading.cli(id))
            .filter(|cli| cli.needs_you())
    }

    /// What the page shows, derived once ([`AiProjection`]).
    #[must_use]
    pub(crate) fn projection(&self, observed: bool) -> AiProjection {
        let choices = &self.choices;
        AiProjection {
            observed,
            enabled: self.enabled,
            looking: self.looking,
            health: self.health(observed),
            clis: self.cli_rows(),
            preferred_from_fleet: from_fleet(choices.preferred.1),
            review_rows: self.review_rows(),
            fleet_rows: self.fleet_rows(),
            fleet_from_url: choices
                .fleet_source
                .as_deref()
                .is_some_and(aterm_agent::ai::fleet::is_url),
        }
    }

    fn health(&self, observed: bool) -> String {
        if !observed {
            return "Reading\u{2026}".to_string();
        }
        if !self.enabled {
            return "Off: aterm does not look at the AI CLIs here; the aterm ai commands still work"
                .to_string();
        }
        let Some(reading) = &self.reading else {
            return match (&self.unavailable, self.looking) {
                (_, true) => "Looking at the AI CLIs\u{2026}".to_string(),
                (Some(why), false) => format!("Not looked at: {}", one_line(why)),
                (None, false) => "Not looked at yet".to_string(),
            };
        };
        let when = crate::harness_screen::read_at_words(
            reading.as_of.saturating_mul(1000),
            reading.offset_s,
        );
        if self.looking {
            return format!("Looking at the AI CLIs\u{2026} \u{b7} last observed {when}");
        }
        if let Some(why) = &self.unavailable {
            return format!(
                "Last look failed: {} \u{b7} last observed {when}",
                one_line(why)
            );
        }
        let needing: Vec<&str> = reading
            .clis
            .iter()
            .filter(|cli| cli.needs_you() && self.choices.wants(cli.id))
            .map(|cli| cli.product)
            .collect();
        let words = match needing.as_slice() {
            [] => "No AI CLI needs you".to_string(),
            [one] => format!("{one} needs you"),
            many => format!("{} need you", listed(many)),
        };
        format!("{words} \u{b7} looked {when}")
    }

    fn cli_rows(&self) -> Vec<AiCliRow> {
        let preferred = self.choices.preferred.0;
        connector::connections()
            .map(|c| {
                let use_from_fleet = self
                    .choices
                    .uses
                    .iter()
                    .any(|(id, _, from)| *id == c.id && from_fleet(*from));
                let tag = if c.id == preferred {
                    " \u{b7} preferred"
                } else {
                    ""
                };
                let reading = self
                    .reading
                    .as_ref()
                    .filter(|_| self.enabled)
                    .and_then(|r| r.cli(c.id));
                match reading {
                    Some(cli) => AiCliRow {
                        id: c.id,
                        product: c.product,
                        title: match &cli.version {
                            Some(version) => format!("{} {version}{tag}", c.product),
                            None => format!("{}{tag}", c.product),
                        },
                        detail: detail_words(cli.state.word(), &cli.fact),
                        path: cli.path.clone(),
                        act: self.actionable_cli(c.id).and_then(AiCli::act),
                        use_from_fleet,
                    },
                    None => AiCliRow {
                        id: c.id,
                        product: c.product,
                        title: format!("{}{tag}", c.product),
                        detail: if !self.enabled {
                            "not looked at: Look for AI CLI problems is off".to_string()
                        } else if self.looking {
                            "looking\u{2026}".to_string()
                        } else {
                            "not looked at yet".to_string()
                        },
                        path: None,
                        act: None,
                        use_from_fleet,
                    },
                }
            })
            .collect()
    }

    fn review_rows(&self) -> Vec<(String, String)> {
        let c = &self.choices;
        let row = |label: &str, value: String| (label.to_string(), value);
        if !c.reviews.0 {
            return vec![row(
                "Reviewers",
                format!("none: reviews are off ({})", from_words(c.reviews.1)),
            )];
        }
        let used: Vec<&str> = c
            .reviewers
            .0
            .iter()
            .copied()
            .filter(|id| c.wants(id))
            .collect();
        let who = match (used.as_slice(), c.reviewers.1) {
            ([], _) => "none: no AI CLI is used here".to_string(),
            (_, From::Default) => "every AI CLI used here but the one asking".to_string(),
            (list, from) => format!(
                "{}, but the one asking ({})",
                list.join(", "),
                from_words(from)
            ),
        };
        let level = match c.review_permissions.0 {
            Choice::Inherit => "the asking agent's own; yolo when you ask".to_string(),
            Choice::Fixed(level) => format!(
                "{} for every reviewer ({})",
                level.word(),
                from_words(c.review_permissions.1)
            ),
        };
        vec![
            row("Reviewers", who),
            row("Permissions", level),
            row(
                "Each review",
                format!(
                    "up to {} {}, up to {} s each, with all their tools",
                    c.review_count,
                    if c.review_count == 1 {
                        "reviewer"
                    } else {
                        "reviewers"
                    },
                    c.review_timeout_s
                ),
            ),
        ]
    }

    fn fleet_rows(&self) -> Vec<(String, String)> {
        let c = &self.choices;
        let row = |label: &str, value: String| (label.to_string(), value);
        let mut rows = vec![row(
            "Machine",
            c.machine
                .clone()
                .unwrap_or_else(|| "not named ([ai] machine)".to_string()),
        )];
        rows.push(row(
            "Signer",
            c.fleet_key.as_deref().map_or_else(
                || "none pinned".to_string(),
                |key| format!("{}\u{2026}", aterm_agent::ai::fleet::fingerprint(key)),
            ),
        ));
        match &c.fleet {
            FleetState::None => rows.push(row("Document", "none".to_string())),
            FleetState::Read { source, row: own } => {
                rows.push(row("Document", source.clone()));
                rows.push(row(
                    "State",
                    if *own {
                        "read; it has a row for this machine".to_string()
                    } else {
                        "read; its top level applies".to_string()
                    },
                ));
            }
            FleetState::Refused { source, why } => {
                rows.push(row("Document", source.clone()));
                rows.push(row("State", format!("refused: {why}")));
            }
        }
        if let Some(held) = self.reading.as_ref().and_then(|r| r.held.as_ref()) {
            let offset = self.reading.as_ref().and_then(|r| r.offset_s);
            let at = |t: Option<i64>| {
                t.map_or_else(
                    || "never".to_string(),
                    |t| crate::harness_screen::read_at_words(t.saturating_mul(1000), offset),
                )
            };
            rows.push(row(
                "Held",
                format!(
                    "seq {} \u{b7} fetched {}",
                    held.seq.map_or_else(|| "?".to_string(), |n| n.to_string()),
                    at(held.fetched_at)
                ),
            ));
            if let Some(error) = &held.error {
                rows.push(row(
                    "Last fetch",
                    format!("{error} (tried {})", at(held.tried_at)),
                ));
            }
        }
        let reading_note = self.reading.as_ref().and_then(|r| r.store_note.clone());
        for note in c.notes.iter().cloned().chain(reading_note) {
            rows.push(row("Note", note));
        }
        rows
    }
}

/// THE AI LINES (`Family::Ai`, a level family).
#[must_use]
pub(crate) fn ai_entries(reading: &AiReading, accounts_listed: bool) -> Vec<Entry> {
    reading
        .clis
        .iter()
        .filter(|cli| cli.needs_you())
        .filter(|cli| {
            !cli.claude
                || (!accounts_listed && matches!(cli.state, State::SignedOut | State::NotInstalled))
        })
        .map(|cli| ai_entry(cli, cli.id == reading.preferred))
        .collect()
}

fn ai_entry(cli: &AiCli, preferred: bool) -> Entry {
    let product = cli.product;
    let (what, reason, step) = match cli.state {
        State::NotInstalled => (
            format!("{product} is not installed"),
            format!("{product} is wanted on this machine, and installing it is your choice"),
            format!("Install {product} in Settings"),
        ),
        State::Due(_) => (
            format!("{product} login due for renewal"),
            "A login ends on a date its vendor sets, and only a sign-in in your browser renews it"
                .to_string(),
            format!("Sign in to {product} in Settings"),
        ),
        _ => (
            format!("{product} is signed out"),
            format!("Only you can sign in to {product}, in a terminal and a browser"),
            format!("Sign in to {product} in Settings"),
        ),
    };
    Entry::new(
        Family::Ai,
        cli.id,
        cli.state.word(),
        &what,
        Why::OnlyYou(Reason::new(reason)),
        Then::press(Mover::open_settings(SettingsRoute::Ai.path()), step),
    )
    .severity(if preferred {
        Severity::Warn
    } else {
        Severity::Info
    })
    .resurface(Resurface::WAIT)
    .more(&cli.fact)
    .more("In a terminal, aterm ai doctor signs in every AI CLI that needs it")
    .log_line(format!("ai need={} cli={}", cli.state.word(), cli.id))
}

/// What the window asks of its look worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiAsk {
    /// Look now (and keep looking: `[ai] enabled` is on).
    Look(u64),
    /// Fetch the fleet document from its URL now, then look (the page's Fetch now).
    Fetch(u64),
    /// Take no look until asked again (`[ai] enabled = false`).
    Off,
}

fn wake_stamp(path: Option<&Path>) -> Option<(Option<std::time::SystemTime>, u64)> {
    let meta = std::fs::metadata(path?).ok()?;
    #[cfg(unix)]
    let file = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let file = meta.len();
    Some((meta.modified().ok(), file))
}

fn look_loop(
    asks: &Receiver<AiAsk>,
    mut on: bool,
    mut generation: u64,
    wake: Option<&Path>,
    (every, poll): (Duration, Duration),
    look: &dyn Fn(bool) -> Result<AiReading, String>,
    send: &dyn Fn(u64, Result<AiReading, String>) -> bool,
) {
    let mut want = on;
    let mut fetch = false;
    let mut last: Option<Instant> = None;
    let mut seen = wake_stamp(wake);
    loop {
        if want && on {
            want = false;
            last = Some(Instant::now());
            if !send(generation, look(std::mem::take(&mut fetch))) {
                return;
            }
        }
        match asks.recv_timeout(poll) {
            Ok(AiAsk::Look(next)) => {
                generation = next;
                on = true;
                want = true;
            }
            Ok(AiAsk::Fetch(next)) => {
                generation = next;
                on = true;
                want = true;
                fetch = true;
            }
            Ok(AiAsk::Off) => on = false,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let stamp = wake_stamp(wake);
        if stamp != seen {
            seen = stamp;
            want = true;
        }
        if last.is_none_or(|at| at.elapsed() >= every) {
            want = true;
        }
    }
}

fn look_now(fetch: bool) -> Result<AiReading, String> {
    let mut env = aterm_agent::ai::cli::Env::from_process();
    env.vars
        .retain(|(name, _)| !aterm_types::domain::is_ai_env_var(name));
    env.harness_state = crate::harness_host::harness_state();
    let _ = aterm_agent::ai::cli::fleet_fetch(&env, &aterm_agent::ai::fleet::LiveFetcher, fetch);
    let look = aterm_agent::ai::cli::look(&env);
    Ok(AiReading::of(
        &look,
        env.now,
        crate::presence::local_offset_at(env.now),
    ))
}

/// The POSIX line typed into a fresh shell to run `exe` with fixed `args`.
#[cfg(any(not(windows), test))]
#[must_use]
pub(crate) fn launch_line(exe: &Path, args: &[&str]) -> Option<String> {
    let exe = exe.to_str()?;
    if exe.chars().any(char::is_control) {
        return None;
    }
    Some(format!(
        "'{}' {}",
        exe.replace('\'', "'\\''"),
        args.join(" ")
    ))
}

/// A Windows invocation for the shell the spawn resolved.
#[cfg(any(windows, test))]
fn windows_launch_line(exe: &Path, args: &[&str], shell: &Path) -> Option<String> {
    let exe = exe.to_str()?;
    if exe.chars().any(char::is_control) || exe.contains('"') {
        return None;
    }
    let name = shell
        .to_str()?
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    let program = match name.strip_suffix(".exe").unwrap_or(&name) {
        "cmd" => {
            // cmd expands both forms even inside quotes (delayed expansion
            // can be enabled by shell_args). Do not type a different path.
            if exe.contains(['%', '!']) {
                return None;
            }
            format!("\"{exe}\"")
        }
        "pwsh" | "powershell" => {
            let mut quoted = String::from("& '");
            for c in exe.chars() {
                // PowerShell accepts typographic single quotes as delimiters
                // too, and doubles any member of this class to quote it.
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    quoted.push(c);
                }
                quoted.push(c);
            }
            quoted.push('\'');
            quoted
        }
        _ => return None,
    };
    Some(format!("{program} {}", args.join(" ")))
}

/// What the App keeps for the AI CLIs.
#[derive(Debug, Default)]
pub(crate) struct AiScreen {
    /// The worker's ask line, once it runs.
    asks: Option<Sender<AiAsk>>,
    /// The newest requested policy/look; older completions own no state.
    generation: u64,
    /// The newest reading.
    pub(crate) reading: Option<AiReading>,
    /// A look is running.
    pub(crate) looking: bool,
    /// Why no look could be taken.
    pub(crate) unavailable: Option<String>,
    /// Whether accounts v2's roster lists rows (`AccountsSaid::Listed`).
    pub(crate) accounts_listed: bool,
}

impl App {
    /// THE SNAPSHOT Settings ▸ Harness ▸ AI CLIs renders.
    #[must_use]
    pub(crate) fn ai_state(&self) -> AiState {
        let enabled = self.config.ai.enabled();
        AiState {
            enabled,
            looking: enabled && self.ai_screen.looking,
            choices: AiChoices::of(&self.config.ai.settings()),
            reading: self.ai_screen.reading.clone(),
            unavailable: self.ai_screen.unavailable.clone(),
        }
    }

    /// Start the AI CLI look worker thread once ([`look_loop`]).
    pub(crate) fn start_ai_looks(&mut self) {
        if self.ai_screen.asks.is_some() {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let on = self.config.ai.enabled();
        let generation = self.ai_screen.generation;
        let wake = aterm_types::dirs::state_dir()
            .map(|root| aterm_agent::ai::store::Store::at(&root).wake_path());
        let (asks, asked) = std::sync::mpsc::channel();
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-ai-look",
            crate::qos::Role::Background,
            move || {
                let look = |fetch: bool| {
                    std::panic::catch_unwind(|| look_now(fetch))
                        .unwrap_or_else(|_| Err("the look failed".to_string()))
                };
                let send = |generation, result| {
                    proxy
                        .send_event(crate::Wake::AiLooked {
                            generation,
                            result: Box::new(result),
                        })
                        .is_ok()
                };
                look_loop(
                    &asked,
                    on,
                    generation,
                    wake.as_deref(),
                    (LOOK_EVERY, WAKE_POLL),
                    &look,
                    &send,
                );
            },
        );
        match spawned {
            Ok(_) => {
                self.ai_screen.asks = Some(asks);
                self.ai_screen.looking = on;
            }
            Err(error) => {
                self.ai_screen.unavailable = Some(format!("the look could not start: {error}"));
            }
        }
        if !on {
            self.clear_ai_entries();
        }
    }

    /// Ask for a look now (the page's Refresh and arrival), and republish so Refresh shows busy.
    pub(crate) fn ask_ai_look(&mut self) {
        if self.config.ai.enabled()
            && let Some(asks) = &self.ai_screen.asks
        {
            self.ai_screen.generation = self.ai_screen.generation.wrapping_add(1);
            self.ai_screen.looking = asks.send(AiAsk::Look(self.ai_screen.generation)).is_ok();
        }
        self.publish_native_harness_state();
    }

    /// Fetch the fleet document from its URL now, then look, and republish so the page shows busy.
    pub(crate) fn ask_ai_fetch(&mut self) {
        if self.config.ai.enabled()
            && let Some(asks) = &self.ai_screen.asks
        {
            self.ai_screen.generation = self.ai_screen.generation.wrapping_add(1);
            self.ai_screen.looking = asks.send(AiAsk::Fetch(self.ai_screen.generation)).is_ok();
        }
        self.publish_native_harness_state();
    }

    /// `[ai]` or its fleet document changed (the config reload).
    pub(crate) fn ai_config_changed(&mut self) {
        let on = self.config.ai.enabled();
        self.ai_screen.generation = self.ai_screen.generation.wrapping_add(1);
        self.ai_screen.reading = None;
        self.ai_screen.unavailable = None;
        self.ai_screen.looking = false;
        if let Some(asks) = &self.ai_screen.asks {
            let sent = asks.send(if on {
                AiAsk::Look(self.ai_screen.generation)
            } else {
                AiAsk::Off
            });
            self.ai_screen.looking = on && sent.is_ok();
        }
        self.clear_ai_entries();
        self.publish_native_harness_state();
    }

    /// A look landed: kept, its lines handed to the digest, and the page republished.
    pub(crate) fn finish_ai_look(&mut self, generation: u64, result: Result<AiReading, String>) {
        if generation != self.ai_screen.generation {
            return;
        }
        self.ai_screen.looking = false;
        if self.config.ai.enabled() {
            match result {
                Ok(reading) => {
                    self.ai_screen.reading = Some(reading);
                    self.ai_screen.unavailable = None;
                }
                Err(why) => self.ai_screen.unavailable = Some(why),
            }
            self.hand_ai_entries();
        }
        self.publish_native_harness_state();
    }

    /// Whether accounts v2's roster lists rows moved.
    pub(crate) fn note_ai_accounts_listed(&mut self, listed: bool) {
        if self.ai_screen.accounts_listed != listed {
            self.ai_screen.accounts_listed = listed;
            self.hand_ai_entries();
        }
    }

    /// Replace the digest's [`Family::Ai`] entries from the newest reading.
    pub(crate) fn hand_ai_entries(&mut self) {
        if !self.config.ai.enabled() {
            self.clear_ai_entries();
            return;
        }
        let Some(reading) = &self.ai_screen.reading else {
            return;
        };
        let entries = ai_entries(reading, self.ai_screen.accounts_listed);
        self.except_replace(Family::Ai, &entries);
    }

    fn clear_ai_entries(&mut self) {
        for key in self.messages.digest_keys_under(&Family::Ai.prefix()) {
            self.except_clear(&key);
        }
    }

    /// SIGN IN (OR INSTALL) ONE AI CLI.
    pub(crate) fn ai_sign_in(&mut self, window: crate::WindowId, id: &str) {
        let Some(c) = connector::connector(id) else {
            aterm_log::warn!("ai: no AI CLI is named {id:?}; nothing opened");
            return;
        };
        let state = self.ai_state();
        let args: [&str; 3] = match state.actionable_cli(c.id).and_then(AiCli::act) {
            Some(AiAct::Install) => ["pkg", "install", c.id],
            Some(AiAct::SignIn) => ["ai", "login", c.id],
            None => {
                aterm_log::warn!("ai: {id} does not need an action under the current policy");
                return;
            }
        };
        let Some(line) = std::env::current_exe().ok().and_then(|exe| {
            #[cfg(windows)]
            {
                let (shell, _) = aterm_pty::select_shell_with_origin(
                    self.session_factory
                        .shell_override
                        .as_deref()
                        .map(std::ffi::OsStr::new),
                );
                windows_launch_line(&exe, &args, Path::new(&shell))
            }
            #[cfg(not(windows))]
            launch_line(&exe, &args)
        }) else {
            aterm_log::warn!(
                "ai: this program path or selected shell has no supported sign-in invocation; nothing opened"
            );
            return;
        };
        aterm_log::info!("ai: a tab opens to run `aterm {}`", args.join(" "));
        self.open_off_loop(
            window,
            crate::tab_spawn::SpawnPlace::Tab,
            crate::tab_spawn::SpawnAsk::default(),
            crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| match landed {
                Ok(sid) => app.type_into_new_session(&sid, &line),
                Err(why) => aterm_log::warn!("ai: the sign-in tab did not open: {why}"),
            })),
        );
    }

    fn type_into_new_session(&mut self, sid: &str, line: &str) {
        let found = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .by_sid(&aterm_session::SessionId::new(sid))
                .map(|handle| (handle.term.clone(), handle.ctx.clone()))
        };
        if let Some((term, ctx)) = found {
            crate::app_tabs::inject_launch_line(&term, &ctx, line);
        }
    }
}

#[cfg(test)]
#[path = "ai_screen_tests.rs"]
mod tests;
