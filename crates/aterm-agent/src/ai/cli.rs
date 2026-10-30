// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `aterm ai` command.

use std::ffi::OsString;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aterm_json::{Map, Number, Value};

use super::change::{self, Base};
use super::config::{self, FleetSource, FleetState, Settings};
use super::connector::{self, Connector};
use super::facts::{self, Facts, Job, LiveRunner, Machine, Runner};
use super::fleet::{self, Fetched, Fetcher, Held, LiveFetcher};
use super::login::{self, Done};
use super::permission::{self, Asker, Choice, Level};
use super::review;
use super::state::{self, Actor, Mark, Row, State};
use super::store::Store;

/// The usage text.
pub const USAGE: &str = "\
aterm ai — this machine's AI CLIs (claude, codex, agy, muse, grok, kimi): health, sign-in, cross-AI reviews.

  aterm ai [status] [--json]    one row per CLI, a health line naming what aterm fixes itself
                                and a NEEDS YOU: line for a sign-in only a person makes (or
                                what a person does first where one cannot start), read-only
                                (exit 0)
  aterm ai doctor [--read-only] [--json] [--on <host>[:/abs/aterm],\u{2026} | --fleet]
                                the same with fixes (not with --read-only or --json): aterm's
                                own, at a terminal or none and under an agent too (installs,
                                and sign-ins through accounts v2's or a pool's renewal), and a
                                CLI's own sign-in only for a person at a terminal; exit 1 while
                                one is signed out or not installed; --on / --fleet ([harness]
                                hosts): those machines too, over ssh, for a person at a terminal
  aterm ai login <cli>          sign one CLI in: accounts v2's or its pool's renewal where a
                                row has an email, else its own sign-in (a person at a terminal)
  aterm ai review [--with <cli>,\u{2026} | --all] [--from <cli>] [--base <ref> | --uncommitted]
                  [--permissions <inherit|yolo|edits|ask|plan>] [--timeout <s>] [--json]
                  [--] [<what to look for> | -]
                                ask the other AI CLIs used here to review this repository's
                                change (the branch's since its upstream, unless --base or
                                --uncommitted), by default at the asking agent's permission level
                                or, when a person asks, yolo ([ai] review_permissions pins one);
                                --with asks those named and --all each of [ai] reviewers, the
                                asker too
  aterm ai review --show <id> [--json]
                                reprint a saved review (kept 24h)
  aterm ai fleet [status]       the fleet document's source, signer, state and fetched copy
  aterm ai fleet fetch          fetch and verify it from the https:// URL `[ai] fleet` names
                                (the window and `aterm ai doctor`, --read-only too, also
                                fetch it, at most every 6h)
  aterm ai fleet keygen <secret-key-file>
                                generate an Ed25519 signing keypair (0600, never overwritten)
  aterm ai fleet sign <doc> --key <secret-key-file>
                                sign <doc> and write <doc>.sig beside it
  aterm ai fleet verify <doc> [--key <public-key>]
                                verify <doc> against <doc>.sig (--key, else [ai] fleet_key)

Config: `[ai]` in `aterm.toml`; the fleet document is `[ai] fleet` (an absolute path or an
https:// URL), else `ai-fleet.toml` beside `aterm.toml`. A repository refuses reviews with
`git config aterm.aiReview false`. See `aterm help ai`.";

/// Everything the verbs read of the process, once.
#[derive(Debug, Clone)]
pub struct Env {
    /// Unix seconds at the start.
    pub now: i64,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// The whole environment.
    pub vars: Vec<(String, String)>,
    /// `aterm.toml`.
    pub config: Option<PathBuf>,
    /// aterm's state root.
    pub aterm_state: Option<PathBuf>,
    /// The harness state root (accounts v2's store is under it).
    pub harness_state: Option<PathBuf>,
    /// atpkg's prefix.
    pub pkg: Option<atpkg::store::Layout>,
    /// `[packages] exclude`.
    pub exclude: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Whether stdin is a terminal.
    pub stdin_tty: bool,
    /// This program.
    pub exe: Option<PathBuf>,
    /// The nearest AI CLI among this process's ancestors, and the permission level its own launch
    /// flags name; incomplete observations remain Unknown.
    pub asker: Asker,
}

impl Env {
    /// The process's own.
    #[must_use]
    pub fn from_process() -> Env {
        use std::io::IsTerminal as _;
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute());
        let aterm_state = aterm_types::dirs::state_dir();
        let harness_state = match std::env::var_os("ATERM_HARNESS_STATE") {
            Some(dir) if Path::new(&dir).is_absolute() => Some(PathBuf::from(dir)),
            Some(_) => None,
            None => aterm_state
                .as_ref()
                .map(|s| s.join("harness").join(crate::harness::cli::HARNESS)),
        };
        let asker = permission::asker(&permission::LiveProcs, &|| {
            let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .filter(|d| d.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(".claude")))?;
            permission::claude_default_mode(&dir)
        });
        Env {
            now: crate::supervise::limit::unix_now(),
            home,
            vars,
            config: crate::harness::cli::default_config_path(),
            aterm_state,
            harness_state,
            pkg: atpkg::store::resolve_configured(),
            exclude: atpkg::config::load().exclude().to_vec(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            stdin_tty: io::stdin().is_terminal(),
            exe: std::env::current_exe().ok(),
            asker,
        }
    }

    /// One variable.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<String> {
        self.vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// The AI store.
    #[must_use]
    pub fn store(&self) -> Option<Store> {
        self.aterm_state.as_deref().map(Store::at)
    }

    /// The AI store's directory, where a URL's fleet document is held.
    #[must_use]
    pub fn ai_dir(&self) -> Option<PathBuf> {
        self.aterm_state
            .as_deref()
            .map(|s| s.join(super::store::DIR))
    }
}

/// One look at every CLI.
#[derive(Debug, Clone)]
pub struct Look {
    /// The configuration, resolved.
    pub settings: Settings,
    /// The facts, in `VENDORS` order.
    pub facts: Vec<Facts>,
    /// The rows, the preferred CLI first.
    pub rows: Vec<Row>,
    /// Why the AI store's record was not used, if it was not.
    pub store_note: Option<String>,
    /// The asking agent, as [`Env::asker`].
    pub asker: Asker,
    /// What is held of a fleet document from a URL, when `[ai] fleet` names one.
    pub held: Option<Held>,
}

fn inherited(asker: Asker) -> Result<(Level, String), &'static str> {
    match asker {
        Asker::Agent(id, level) => Ok((level, format!("inherited from {id}"))),
        Asker::Person => Ok((Level::Yolo, "a person asks".to_string())),
        Asker::Unknown => {
            Err("the asking agent's permissions are unknown; choose --permissions explicitly")
        }
    }
}

/// The level reviewers run at, or why inherited permissions are not proven.
pub fn review_level(s: &Settings, asker: Asker) -> Result<(Level, String), &'static str> {
    match s.review_permissions.0 {
        Choice::Fixed(level) => Ok((level, format!("set in {}", s.review_permissions.1.word()))),
        Choice::Inherit => inherited(asker),
    }
}

fn held_of(s: &Settings, ai_dir: Option<&Path>) -> Option<Held> {
    let url = s.fleet_source.as_deref().filter(|u| fleet::is_url(u))?;
    let key = s.fleet_key.as_deref().unwrap_or_default();
    fleet::held_state(ai_dir?).filter(|h| h.of(url, key))
}

/// Fetches the fleet document from the URL `[ai] fleet` names.
pub fn fleet_fetch(env: &Env, fetcher: &dyn Fetcher, force: bool) -> Option<Fetched> {
    let text = env.config.as_deref().and_then(|p| {
        crate::harness::accounts::read_bounded(p, config::CONFIG_MAX_BYTES)
            .ok()
            .flatten()
    });
    let (local, _) = config::read(text.as_deref().unwrap_or_default());
    let Some(FleetSource::Url(url)) = local.fleet else {
        return None;
    };
    let Some(dir) = env.ai_dir() else {
        return Some(Fetched::Refused(
            "aterm's state directory is unknown".to_string(),
        ));
    };
    let key = local.fleet_key.unwrap_or_default();
    Some(fleet::fetch(&dir, &url, &key, env.now, fetcher, force))
}

/// Looks at every CLI.
#[must_use]
pub fn look(env: &Env) -> Look {
    let ai_dir = env.ai_dir();
    let config_text = env.config.as_deref().and_then(|p| {
        crate::harness::accounts::read_bounded(p, config::CONFIG_MAX_BYTES)
            .ok()
            .flatten()
    });
    let (settings, parsed_doc) = config::load_from_text(
        config_text.as_deref(),
        env.config.as_deref(),
        ai_dir.as_deref(),
        env.now,
    );
    let held = held_of(&settings, ai_dir.as_deref());
    let store = env.store();
    let (records, store_note) = match store.as_ref().map(Store::load) {
        Some(Ok(r)) => (r, None),
        Some(Err(super::store::Unread::Torn(why))) => (
            Default::default(),
            Some(format!(
                "the AI store's connections.json is not used ({why})"
            )),
        ),
        Some(Err(super::store::Unread::NotOurs(n))) => (
            Default::default(),
            Some(format!(
                "the AI store's connections.json has schema {n}, which this build does not read; \
                 it is not used"
            )),
        ),
        None => (
            Default::default(),
            Some("aterm's state directory is unknown".to_string()),
        ),
    };
    let var = |k: &str| env.var(k);
    let roster = env
        .config
        .as_deref()
        .and_then(crate::harness::accounts::roster_beside);
    let has_other_pool = roster
        .as_deref()
        .and_then(Path::parent)
        .is_some_and(|config_dir| {
            [
                crate::harness::accounts::Cli::Codex,
                crate::harness::accounts::Cli::Grok,
                crate::harness::accounts::Cli::Kimi,
            ]
            .into_iter()
            .any(|cli| config_dir.join(cli.accounts().roster_file).is_file())
        });
    let accounts_mode = Some(match (has_other_pool, config_text.as_deref()) {
        (true, Some(text)) => {
            crate::supervise::config::SupervisorConfig::from_parsed_aterm_toml(
                parsed_doc.as_ref(),
                text,
            )
            .0
            .accounts
        }
        _ => crate::supervise::config::SupervisorConfig::default().accounts,
    });
    let machine = Machine {
        now: env.now,
        home: env.home.clone(),
        var: &var,
        pkg: env.pkg.clone(),
        exclude: env.exclude.clone(),
        records,
        harness_state: env.harness_state.clone(),
        roster,
        accounts_mode,
        config: env.config.clone(),
    };
    let facts: Vec<Facts> = connector::connections()
        .map(|c| facts::gather(&machine, c))
        .collect();
    let mut settings = settings;
    let view = facts.iter().find_map(|f| f.accounts.as_ref());
    if let Some(why) = view.and_then(|a| a.refused.as_deref()) {
        settings.notes.push(format!(
            "accounts.toml is refused ({why}), so Claude reads as having no roster; \
             `aterm harness accounts` says how to fix it"
        ));
    } else if let Some(why) = view.and_then(|a| a.unread.as_deref()) {
        settings.notes.push(format!(
            "accounts v2's records cannot be read ({why}), so Claude reads as having no roster"
        ));
    }
    let mut rows: Vec<Row> = facts
        .iter()
        .map(|f| state::assess(f, settings.wants(f.connector.id), env.now))
        .collect();
    let preferred = settings.preferred.0.id;
    rows.sort_by_key(|r| r.connector.id != preferred);
    Look {
        settings,
        facts,
        rows,
        store_note,
        asker: env.asker,
        held,
    }
}

const LABEL_W: usize = 12;
const WORDS_MAX: usize = 16 * 1024;
const STATE_W: usize = 15;

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

/// The most of a row's fact the text says: `--json`'s `fact` carries it whole.
const FACT_MAX: usize = 400;

/// What a clipped fact ends with ([`fact_line`]).
const FACT_MORE: &str = "\u{2026} (`--json` says it whole)";

/// A row's fact as the text says it, fit for a terminal ([`review::sanitize_block`]) on one
/// line of at most [`FACT_MAX`] bytes: where it is longer, cut after its last whole word that fits
/// before [`FACT_MORE`], so a word (a refusal's reason among them) is never cut short unmarked.
fn fact_line(fact: &str) -> String {
    let clean = review::sanitize_block(fact, fact.len()).replace('\n', " ");
    if clean.len() <= FACT_MAX {
        return clean;
    }
    let room = FACT_MAX - FACT_MORE.len();
    let fits = (0..=room)
        .rev()
        .find(|&i| clean.is_char_boundary(i))
        .unwrap_or(0);
    // The cut falls at a space that ends a word; a first word longer than the room is cut at
    // a character instead.
    let cut = if clean[fits..].starts_with(' ') {
        fits
    } else {
        clean[..fits].rfind(' ').unwrap_or(fits)
    };
    let head = clean[..cut].trim_end_matches([' ', '\u{b7}', ',', ';', ':']);
    format!("{head}{FACT_MORE}")
}

/// The doctor's text: each row, the reviews and fleet lines, the notes, the `health:` line with
/// `next`, and the `NEEDS YOU:` line for a sign-in only a person makes
/// ([`state::Health::needs_you`]).
pub fn render_text(out: &mut dyn Write, look: &Look, fixes: bool, next: &str) {
    render_look(out, look, fixes, next, true);
}

/// [`render_text`], the `NEEDS YOU:` line only where `needs_you` asks for it (not before the
/// doctor's own fixes, which may make it moot).
fn render_look(out: &mut dyn Write, look: &Look, fixes: bool, next: &str, needs_you: bool) {
    let s = &look.settings;
    let from = |f: config::From| match f {
        config::From::Default => String::new(),
        other => format!(" ({})", other.word()),
    };
    let _ = writeln!(
        out,
        "{}{}{}",
        pad("preferred:", LABEL_W),
        s.preferred.0.id,
        from(s.preferred.1)
    );
    for row in &look.rows {
        let fact = fact_line(&row.fact);
        let _ = writeln!(
            out,
            "{}{}{fact}",
            pad(&format!("{}:", row.connector.id), LABEL_W),
            pad(row.state.word(), STATE_W)
        );
        if fixes
            && row.mark >= Mark::Warn
            && let Some(fix) = row.fix.as_ref().or(row.mend.as_ref())
        {
            let _ = writeln!(out, "{}fix: {fix}", " ".repeat(LABEL_W));
        }
    }
    let _ = writeln!(out, "{}{}", pad("reviews:", LABEL_W), reviews_line(look));
    let fleet = match &s.fleet {
        FleetState::None => "none".to_string(),
        FleetState::Read { source, row } => format!(
            "{}{} in {source}{}",
            s.machine.as_deref().unwrap_or("no [ai] machine"),
            if *row { "" } else { " (no row of its own)" },
            held_words(look.held.as_ref(), s.fleet_key.as_deref())
        ),
        FleetState::Refused { source, why } => format!("refused ({why}): {source}"),
    };
    let _ = writeln!(out, "{}{fleet}", pad("fleet:", LABEL_W));
    let fetch_note = look
        .held
        .as_ref()
        .and_then(|h| h.error.as_ref())
        .map(|e| format!("the fleet document's last fetch failed ({e}); what is held stands"));
    for note in s
        .notes
        .iter()
        .chain(look.store_note.iter())
        .chain(fetch_note.iter())
    {
        let _ = writeln!(
            out,
            "{}{}",
            pad("note:", LABEL_W),
            review::sanitize_block(note, 300).replace('\n', " ")
        );
    }
    let h = state::health(&look.rows);
    let _ = writeln!(out, "\nhealth: {}", h.line(next));
    if needs_you && let Some(line) = h.needs_you() {
        let _ = writeln!(out, "{line}");
    }
}

/// Whether a review asks `id` unless named: wanted here, and its row not off ([`State::Off`]: no
/// build for this platform, excluded, or uninstalled on purpose), which a review would only skip.
fn asked_by_default(s: &Settings, rows: &[Row], id: &str) -> bool {
    s.wants(id)
        && rows
            .iter()
            .find(|r| r.connector.id == id)
            .is_none_or(|r| r.state != State::Off)
}

fn default_reviewers<'s>(s: &'s Settings, rows: &[Row], asker: &str) -> Vec<&'s str> {
    s.reviewers
        .0
        .iter()
        .filter(|c| asked_by_default(s, rows, c.id) && c.id != asker)
        .map(|c| c.id)
        .collect()
}

/// The asking CLI when `--from` names none: the ancestor agent, else `CLAUDECODE` /
/// `CODEX_THREAD_ID`, else the preferred CLI.
fn asker_id(env: &Env, s: &Settings) -> &'static str {
    if let Some((id, _)) = env.asker.agent() {
        return id;
    }
    if env.var("CLAUDECODE").is_some() {
        "claude"
    } else if env.var("CODEX_THREAD_ID").is_some() {
        "codex"
    } else {
        s.preferred.0.id
    }
}

fn reviews_line(look: &Look) -> String {
    let s = &look.settings;
    if !s.reviews.0 {
        return format!("off (reviews = false, set in {})", s.reviews.1.word());
    }
    let asker = look.asker.agent().map_or(s.preferred.0.id, |(id, _)| id);
    let who = default_reviewers(s, &look.rows, asker);
    let (level, why) = review_level(s, look.asker)
        .map(|(level, why)| (level.word().to_string(), why))
        .unwrap_or_else(|why| ("unknown".to_string(), why.to_string()));
    let who = if who.is_empty() {
        "nobody: no other AI CLI is used here".to_string()
    } else {
        who.join(", ")
    };
    format!(
        "on · {who} (when {asker} asks) · up to {} per review · {} s · {} ({why})",
        s.review_count.0,
        s.review_timeout_s.0,
        level.as_str()
    )
}

fn held_words(held: Option<&Held>, key: Option<&str>) -> String {
    let signer = key.map_or(String::new(), |k| {
        format!(" · signer {}…", fleet::fingerprint(k))
    });
    match held {
        Some(h) if h.fetched_at.is_some() => format!(
            " · seq {} · fetched {}{signer}",
            h.seq.map_or("?".to_string(), |n| n.to_string()),
            h.fetched_at.map_or(String::new(), state::when),
        ),
        _ => signer,
    }
}

fn int(n: i64) -> Value {
    Value::Number(Number::from(n))
}

fn opt_int(n: Option<i64>) -> Value {
    n.map_or(Value::Null, int)
}

fn rfc(n: Option<i64>) -> Value {
    n.and_then(|t| u64::try_from(t).ok())
        .map_or(Value::Null, |t| {
            Value::from(aterm_types::rfc3339::format_rfc3339(t))
        })
}

/// The doctor's one JSON object.
#[must_use]
pub fn render_json(look: &Look, now: i64, next: &str) -> String {
    let s = &look.settings;
    let mut clis = Vec::new();
    for row in &look.rows {
        let use_from = s
            .uses
            .iter()
            .find(|(c, _, _)| c.id == row.connector.id)
            .map_or("default", |(_, _, f)| f.word());
        let mut o = Map::new();
        let text = |v: &Option<String>| v.as_ref().map_or(Value::Null, |s| Value::from(s.as_str()));
        o.insert("id".to_string(), Value::from(row.connector.id));
        o.insert("product".to_string(), Value::from(row.connector.product));
        o.insert("use".to_string(), Value::Bool(row.wanted));
        o.insert("use_from".to_string(), Value::from(use_from));
        o.insert("state".to_string(), Value::from(row.state.word()));
        o.insert("mark".to_string(), Value::from(row.mark.word()));
        o.insert("measured".to_string(), Value::Bool(row.connector.measured));
        o.insert("version".to_string(), text(&row.version));
        o.insert("plan".to_string(), text(&row.plan));
        o.insert("used_at".to_string(), opt_int(row.used.map(|u| u.0)));
        o.insert("used_at_rfc3339".to_string(), rfc(row.used.map(|u| u.0)));
        o.insert(
            "used_how".to_string(),
            row.used.map_or(Value::Null, |u| Value::from(u.1)),
        );
        o.insert("renew_by".to_string(), opt_int(row.renew_by));
        o.insert("renew_by_rfc3339".to_string(), rfc(row.renew_by));
        o.insert("limited_until".to_string(), opt_int(row.limited_until));
        o.insert("limited_until_rfc3339".to_string(), rfc(row.limited_until));
        o.insert("fact".to_string(), Value::from(row.fact.as_str()));
        o.insert("fix".to_string(), text(&row.fix));
        o.insert("mend".to_string(), text(&row.mend));
        o.insert(
            "fix_by".to_string(),
            row.actor.map_or(Value::Null, |a| Value::from(a.word())),
        );
        clis.push(Value::Object(o));
    }
    let h = state::health(&look.rows);
    let mut health = Map::new();
    health.insert("word".to_string(), Value::from(h.word));
    health.insert(
        "problems".to_string(),
        Value::Array(h.failing.iter().map(|i| Value::from(*i)).collect()),
    );
    health.insert(
        "next".to_string(),
        if h.word == "OK" {
            Value::Null
        } else {
            Value::from(next)
        },
    );
    health.insert(
        "needs_you".to_string(),
        h.needs_you().map_or(Value::Null, Value::from),
    );
    let asker = look.asker.agent().map_or(s.preferred.0.id, |(id, _)| id);
    let (level, level_why) = review_level(s, look.asker)
        .map(|(level, why)| (level.word().to_string(), why))
        .unwrap_or_else(|why| ("unknown".to_string(), why.to_string()));
    let mut reviews = Map::new();
    reviews.insert("on".to_string(), Value::Bool(s.reviews.0));
    reviews.insert("on_from".to_string(), Value::from(s.reviews.1.word()));
    reviews.insert(
        "reviewers".to_string(),
        Value::Array(s.reviewers.0.iter().map(|c| Value::from(c.id)).collect()),
    );
    reviews.insert("from".to_string(), Value::from(s.reviewers.1.word()));
    reviews.insert("asker".to_string(), Value::from(asker));
    reviews.insert(
        "asks".to_string(),
        Value::Array(
            default_reviewers(s, &look.rows, asker)
                .into_iter()
                .map(Value::from)
                .collect(),
        ),
    );
    reviews.insert("count".to_string(), int(i64::from(s.review_count.0)));
    reviews.insert(
        "timeout_s".to_string(),
        int(i64::from(s.review_timeout_s.0)),
    );
    reviews.insert(
        "permissions".to_string(),
        Value::from(s.review_permissions.0.word()),
    );
    reviews.insert(
        "permissions_from".to_string(),
        Value::from(s.review_permissions.1.word()),
    );
    reviews.insert("level".to_string(), Value::from(level.as_str()));
    reviews.insert("level_why".to_string(), Value::from(level_why.as_str()));
    let mut fleet = Map::new();
    fleet.insert("state".to_string(), Value::from(s.fleet.word()));
    if let Some(source) = &s.fleet_source {
        fleet.insert("source".to_string(), Value::from(source.as_str()));
    }
    if let Some(key) = &s.fleet_key {
        fleet.insert("key".to_string(), Value::from(key.as_str()));
    }
    if let FleetState::Refused { why, .. } = &s.fleet {
        fleet.insert("why".to_string(), Value::from(why.as_str()));
    }
    if let Some(h) = &look.held {
        fleet.insert("seq".to_string(), opt_int(h.seq));
        fleet.insert("fetched_at".to_string(), opt_int(h.fetched_at));
        fleet.insert("fetched_at_rfc3339".to_string(), rfc(h.fetched_at));
        fleet.insert("tried_at".to_string(), opt_int(h.tried_at));
        fleet.insert("valid_until".to_string(), opt_int(h.valid_until));
        fleet.insert(
            "error".to_string(),
            h.error.as_deref().map_or(Value::Null, Value::from),
        );
    }
    let mut o = Map::new();
    o.insert("schema".to_string(), int(1));
    o.insert("kind".to_string(), Value::from("ai"));
    o.insert("as_of".to_string(), int(now));
    o.insert("as_of_rfc3339".to_string(), rfc(Some(now)));
    o.insert(
        "machine".to_string(),
        s.machine.as_deref().map_or(Value::Null, Value::from),
    );
    o.insert("preferred".to_string(), Value::from(s.preferred.0.id));
    o.insert(
        "preferred_from".to_string(),
        Value::from(s.preferred.1.word()),
    );
    o.insert("fleet".to_string(), Value::Object(fleet));
    o.insert("clis".to_string(), Value::Array(clis));
    o.insert("reviews".to_string(), Value::Object(reviews));
    o.insert(
        "notes".to_string(),
        Value::Array(s.notes.iter().map(|n| Value::from(n.as_str())).collect()),
    );
    o.insert("health".to_string(), Value::Object(health));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
}

/// Whether a person is at this run to make a fix only a person makes ([`Actor::Person`]: a CLI's
/// own sign-in, an email Claude's adoption asks): a terminal on stdin and no agent's marker.
///
/// # Errors
///
/// Why no person is here: stdin is no terminal, or an agent's marker is set.
fn person_here(env: &Env) -> Result<(), String> {
    if !env.stdin_tty {
        return Err("stdin is not a terminal".to_string());
    }
    if let Some(m) = login::agent_marker(&|k| env.var(k)) {
        return Err(format!("{m} is set (an agent runs this)"));
    }
    Ok(())
}

/// Whether `row`'s fix may run here: aterm's own at any run but `--read-only`'s, a person's
/// only where [`person_here`] says one is.
///
/// # Errors
///
/// Why it may not.
fn may_fix(env: &Env, row: &Row) -> Result<(), String> {
    match row.actor {
        Some(Actor::Person) => person_here(env),
        Some(Actor::Aterm) | None => Ok(()),
    }
}

fn ask_line(question: &str) -> Option<String> {
    let mut err = io::stderr();
    let _ = write!(err, "{question}");
    let _ = err.flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

fn sign_in_one(
    env: &Env,
    runner: &dyn Runner,
    store: &Store,
    facts: &Facts,
    row: &Row,
    out: &mut dyn Write,
) -> Done {
    let var = |k: &str| env.var(k);
    let now = || crate::supervise::limit::unix_now();
    let exe = env.exe.clone().unwrap_or_else(|| PathBuf::from("aterm"));
    let host = login::Host {
        runner,
        var: &var,
        vars: &env.vars,
        aterm: &exe,
        store,
        now: &now,
        ask: &ask_line,
        no_desk: Some(Path::new(crate::harness::login_drive::SANDBOX_EXEC)),
        attended: person_here(env).is_ok(),
    };
    login::sign_in(&host, facts, row, out)
}

fn done_line(c: &Connector, done: &Done) -> String {
    done_line_of(c.id, done)
}

fn done_line_of(id: &str, done: &Done) -> String {
    match done {
        Done::SignedIn(Some(m)) => format!("{}: signed in ({m})", id),
        Done::SignedIn(None) => format!("{}: signed in", id),
        Done::NotChecked => format!("{}: the sign-in ended; nothing reads whether it worked", id),
        Done::SignedOut => format!("{}: still signed out", id),
        Done::Failed(why) => format!(
            "{}: did not sign in: {}",
            id,
            crate::harness::one_line(why, 160)
        ),
        // aterm's own sentence, whose quoted parts are bounded where they are quoted: folded onto
        // one line and never cut, since its end is the reason or the next step.
        Done::Refused(why) => format!(
            "{}: not started: {}",
            id,
            crate::supervise::limit::one_line(why)
        ),
    }
}

/// The health line's next act over `look`'s rows ([`state::Health::next`]): what `aterm ai
/// doctor` fixes itself, then the sign-ins only a person makes.
fn next_act(look: &Look) -> String {
    state::health(&look.rows)
        .next()
        .unwrap_or_else(|| "aterm ai doctor".to_string())
}

fn install(env: &Env, runner: &dyn Runner, id: &str) -> Result<(), String> {
    let exe = env.exe.clone().ok_or("this program's path is unknown")?;
    let args = vec!["pkg".to_string(), "install".to_string(), id.to_string()];
    match runner.terminal(&exe, &args, &env.vars, None) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("`aterm pkg install {id}` exited {code}")),
        Err(why) => Err(why),
    }
}

/// `aterm ai doctor` here: the look, then each fix it may make ([`may_fix`]): aterm's own
/// (installs, and sign-ins through accounts v2's or a pool's ladder) at any run but
/// `--read-only`'s or `--json`'s, under an agent and with no terminal too; a person's only where
/// one is at this terminal, else said and not started. The look again after.
fn doctor_here(
    env: &Env,
    runner: &dyn Runner,
    read_only: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let first = look(env);
    let needs: Vec<usize> = first
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.state.needs_act(r.wanted))
        .map(|(i, _)| i)
        .collect();
    if json || read_only || needs.is_empty() {
        let next = next_act(&first);
        if json {
            let _ = writeln!(out, "{}", render_json(&first, env.now, &next));
        } else {
            render_text(out, &first, true, &next);
        }
        return exit_of(&first.rows);
    }
    render_look(out, &first, true, "fixing below", false);
    let Some(store) = env.store() else {
        let _ = writeln!(
            err,
            "aterm ai doctor: aterm's state directory is unknown; nothing was run"
        );
        return ExitCode::from(1);
    };
    // A sign-in that runs nothing wherever it runs ([`login::refusal`]): its reason. A fix only
    // a person makes, with nobody here to make it: said, never started.
    let held = |f: &Facts, row: &Row| {
        login::refusal(f, row).map(Done::Refused).or_else(|| {
            may_fix(env, row).err().map(|why| {
                let id = row.connector.id;
                Done::Refused(format!(
                    "{why}; no route of aterm's signs {id} in, so a person does, once, at this \
                     Mac (`{}`)",
                    row.fix
                        .clone()
                        .unwrap_or_else(|| format!("aterm ai login {id}"))
                ))
            })
        })
    };
    for i in needs {
        let row = &first.rows[i];
        let Some(f) = first
            .facts
            .iter()
            .find(|f| f.connector.id == row.connector.id)
        else {
            continue;
        };
        let _ = writeln!(out, "\n── {} ──", row.connector.product);
        let _ = out.flush();
        if matches!(row.state, State::NotInstalled) {
            if let Err(why) = install(env, runner, row.connector.id) {
                let _ = writeln!(out, "{}: not installed: {why}", row.connector.id);
                continue;
            }
            let again = look(env);
            let (Some(r), Some(f)) = (
                again
                    .rows
                    .iter()
                    .find(|r| r.connector.id == row.connector.id),
                again
                    .facts
                    .iter()
                    .find(|f| f.connector.id == row.connector.id),
            ) else {
                continue;
            };
            if r.state.needs_act(r.wanted) {
                let done =
                    held(f, r).unwrap_or_else(|| sign_in_one(env, runner, &store, f, r, out));
                let _ = writeln!(out, "{}", done_line(f.connector, &done));
            } else {
                let _ = writeln!(out, "{}: installed ({})", r.connector.id, r.fact);
            }
            continue;
        }
        let done = held(f, row).unwrap_or_else(|| sign_in_one(env, runner, &store, f, row, out));
        let _ = writeln!(out, "{}", done_line(f.connector, &done));
    }
    let after = Env {
        now: crate::supervise::limit::unix_now(),
        ..env.clone()
    };
    let last = look(&after);
    let _ = writeln!(out);
    render_text(out, &last, true, &next_act(&last));
    exit_of(&last.rows)
}

fn exit_of(rows: &[Row]) -> ExitCode {
    if !rows.iter().any(|r| r.mark == Mark::Fail) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn machines(env: &Env, on: Option<&[String]>) -> Result<Vec<(String, String)>, String> {
    let hosts = match on {
        Some(on) => on.to_vec(),
        None => {
            crate::supervise::config::SupervisorConfig::from_path(env.config.as_deref())
                .0
                .hosts
        }
    };
    if hosts.is_empty() {
        return Err(
            "no machines: name them with --on <host>,… or list them in [harness] hosts".to_string(),
        );
    }
    crate::harness::cli::machine_entries(&hosts)
}

fn ssh_program() -> PathBuf {
    PathBuf::from("/usr/bin/ssh")
}

fn remote_states(json: &str) -> Option<Vec<(String, String)>> {
    let v: Value = aterm_json::from_str(json).ok()?;
    let mut out = Vec::new();
    for cli in v.get("clis")?.as_array()? {
        let id = cli.get("id")?.as_str()?;
        let word = cli.get("state")?.as_str()?;
        let known = connector::connector(id).is_some()
            && [
                "ok",
                "idle",
                "limited",
                "due",
                "unproven",
                "signed-out",
                "not-installed",
                "off",
            ]
            .contains(&word);
        if known {
            if out.iter().any(|(seen, _)| seen == id) {
                return None;
            }
            out.push((id.to_string(), word.to_string()));
        }
    }
    Some(out)
}

type States = Vec<(String, String)>;

const WORKING: &[&str] = &["ok", "idle", "limited", "due"];

fn fleet_poll(
    env: &Env,
    runner: &dyn Runner,
    hosts: &[(String, String)],
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let here_look = look(env);
    let mut machines: Vec<(String, Option<States>)> = vec![(
        "this machine".to_string(),
        Some(
            here_look
                .rows
                .iter()
                .map(|r| (r.connector.id.to_string(), r.state.word().to_string()))
                .collect(),
        ),
    )];
    for (host, aterm) in hosts {
        let args: Vec<String> = [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
            aterm,
            "ai",
            "doctor",
            "--json",
            "--read-only",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let ran = runner.run(&Job {
            program: &ssh_program(),
            args: &args,
            env: &env.vars,
            stdin: None,
            cwd: None,
            budget: Duration::from_secs(30),
        });
        let states = ran
            .ok()
            .filter(|r| r.stdout.len() <= 64 * 1024)
            .and_then(|r| remote_states(&r.stdout));
        machines.push((host.clone(), states));
    }
    let mut cover: Vec<(&'static str, usize, usize)> =
        connector::connections().map(|c| (c.id, 0, 0)).collect();
    for (_, states) in &machines {
        for (id, word) in states.iter().flatten() {
            if let Some(w) = cover.iter_mut().find(|w| w.0 == id.as_str())
                && word != "off"
            {
                w.2 += 1;
                if WORKING.contains(&word.as_str()) {
                    w.1 += 1;
                }
            }
        }
    }
    let none: Vec<&str> = cover
        .iter()
        .filter(|(_, ok, of)| *of > 0 && *ok == 0)
        .map(|(id, _, _)| *id)
        .collect();
    if json {
        let mut list = Vec::new();
        for (host, states) in &machines {
            let mut m = Map::new();
            m.insert("host".to_string(), Value::from(host.as_str()));
            m.insert(
                "clis".to_string(),
                states.as_ref().map_or(Value::Null, |st| {
                    let mut o = Map::new();
                    for (id, word) in st {
                        o.insert(id.clone(), Value::from(word.as_str()));
                    }
                    Value::Object(o)
                }),
            );
            list.push(Value::Object(m));
        }
        let mut coverage = Map::new();
        for (id, ok, of) in &cover {
            let mut c = Map::new();
            c.insert("working".to_string(), int(i64::try_from(*ok).unwrap_or(0)));
            c.insert("of".to_string(), int(i64::try_from(*of).unwrap_or(0)));
            coverage.insert((*id).to_string(), Value::Object(c));
        }
        let mut o = Map::new();
        o.insert("schema".to_string(), int(1));
        o.insert("kind".to_string(), Value::from("ai-fleet"));
        o.insert("machines".to_string(), Value::Array(list));
        o.insert("coverage".to_string(), Value::Object(coverage));
        o.insert(
            "no_machine_works".to_string(),
            Value::Array(none.iter().map(|i| Value::from(*i)).collect()),
        );
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        render_text(out, &here_look, true, &next_act(&here_look));
        for (host, states) in machines.iter().skip(1) {
            let _ = writeln!(out, "\n{host}:");
            match states {
                Some(st) => {
                    for (id, word) in st {
                        let _ = writeln!(out, "  {}{word}", pad(&format!("{id}:"), LABEL_W));
                    }
                }
                None => {
                    let _ = writeln!(
                        out,
                        "  could not be read (ssh failed, or no usable `ai doctor --json` answer)"
                    );
                }
            }
        }
        let line: Vec<String> = cover
            .iter()
            .map(|(id, ok, of)| format!("{id} {ok}/{of}"))
            .collect();
        let _ = writeln!(out, "\ncoverage: {}", line.join(" · "));
        if !none.is_empty() {
            let _ = writeln!(
                out,
                "health: FAIL — no machine has a working {}",
                none.join(", ")
            );
        }
    }
    let _ = err;
    if none.is_empty() && !here_look.rows.iter().any(|r| r.mark == Mark::Fail) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn doctor_away(
    env: &Env,
    runner: &dyn Runner,
    on: Option<&[String]>,
    read_only: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let hosts = match machines(env, on) {
        Ok(h) => h,
        Err(why) => {
            let _ = writeln!(err, "aterm ai doctor: {why}");
            return ExitCode::from(2);
        }
    };
    // Each machine's doctor runs on its own terminal over `ssh -t`, where a person makes what
    // only a person can; with nobody here it is a poll of each machine's look.
    if json || read_only || person_here(env).is_err() {
        return fleet_poll(env, runner, &hosts, json, out, err);
    }
    let here = doctor_here(env, runner, false, false, out, err);
    let mut code = here;
    for (host, aterm) in &hosts {
        let _ = writeln!(out, "\n══ {host} ══");
        let _ = out.flush();
        let args: Vec<String> = [
            "-t",
            "-L",
            "1455:127.0.0.1:1455",
            "--",
            host,
            aterm,
            "ai",
            "doctor",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        match runner.terminal(&ssh_program(), &args, &env.vars, None) {
            Ok(0) => {}
            Ok(_) => code = ExitCode::from(1),
            Err(why) => {
                let _ = writeln!(err, "aterm ai doctor: ssh {host}: {why}");
                code = ExitCode::from(1);
            }
        }
    }
    code
}

fn csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `aterm ai login <id>`: the connection's sign-in ([`login::sign_in`]). aterm's own (accounts
/// v2's or the pool's renewal) runs at a terminal or none; a person's (the CLI's own sign-in, an
/// email Claude's adoption asks) only at a terminal. A CLI not installed is installed first, as
/// the doctor does. A sign-in that runs nothing wherever it runs ([`login::refusal`]: a row
/// nothing here signs in) is refused with its reason, at a terminal or none. With no terminal, a
/// row whose person's step is a mend ([`state::Row::mend`]: of the pin or roster that refuses its
/// launches, a pinned row's sign-in the pool's does not make, or the sign-in of this machine's own
/// login at a terminal and then its pin, [`state::sign_in_then_pin`]) is told that mend and the
/// refusal, never sent to a sign-in.
fn login_verb(
    env: &Env,
    runner: &dyn Runner,
    id: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    if connector::connector(id).is_none() {
        let _ = writeln!(
            err,
            "aterm ai login: `{}` is no AI CLI ({})",
            crate::harness::one_line(id, 40),
            connector::ids().join(", ")
        );
        return ExitCode::from(2);
    }
    let Some(store) = env.store() else {
        let _ = writeln!(err, "aterm ai login: aterm's state directory is unknown");
        return ExitCode::from(1);
    };
    let mut l = look(env);
    if l.rows
        .iter()
        .any(|r| r.connector.id == id && r.state == State::NotInstalled)
    {
        if let Err(why) = install(env, runner, id) {
            let _ = writeln!(out, "{id}: not installed: {why}");
            return ExitCode::from(1);
        }
        l = look(env);
    }
    let (Some(row), Some(f)) = (
        l.rows.iter().find(|r| r.connector.id == id),
        l.facts.iter().find(|f| f.connector.id == id),
    ) else {
        let _ = writeln!(
            err,
            "aterm ai login: `{id}` is not a connection in this build"
        );
        return ExitCode::from(2);
    };
    // A person is sent to a terminal only where a run there would start something; a sign-in
    // that runs nothing for anyone (a row nothing here signs in) says why instead.
    if row.actor.unwrap_or_else(|| state::sign_in_actor(f)) == Actor::Person
        && !env.stdin_tty
        && login::refusal(f, row).is_none()
    {
        // Where the person's step is a mend ([`state::Row::mend`]: a change to the owner's pin or
        // roster that refuses every launch, or the sign-in of a pinned row the pool's sign-in does
        // not make), the sign-in `aterm ai login` makes does not end the refusal, so that mend is
        // the step. Where the mend is that sign-in at a terminal and then a pin
        // ([`state::sign_in_then_pin`]), it does not end it alone.
        if let Some(mend) = &row.mend {
            let alone = if state::sign_in_then_pin(f).is_some() {
                " alone"
            } else {
                ""
            };
            let refused = f
                .pool
                .as_ref()
                .and_then(|p| p.launch_refused.as_deref())
                .map(|why| {
                    let why = why
                        .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
                        .unwrap_or(why);
                    format!("; a launch of {id} now is refused: {why}")
                })
                .unwrap_or_default();
            let _ = writeln!(
                err,
                "aterm ai login: stdin is not a terminal, and `aterm ai login {id}`{alone} does \
                 not end the refusal of {id}'s launches: a person at this Mac: {mend}{refused}"
            );
            return ExitCode::from(2);
        }
        let _ = writeln!(
            err,
            "aterm ai login: stdin is not a terminal, and no route of aterm's signs {id} in: a \
             person runs `{}` in a terminal at this Mac",
            row.fix
                .clone()
                .unwrap_or_else(|| format!("aterm ai login {id}"))
        );
        return ExitCode::from(2);
    }
    let done = sign_in_one(env, runner, &store, f, row, out);
    let _ = writeln!(out, "{}", done_line(f.connector, &done));
    match done {
        Done::SignedIn(_) | Done::NotChecked => ExitCode::SUCCESS,
        Done::Refused(_) => ExitCode::from(2),
        Done::SignedOut | Done::Failed(_) => ExitCode::from(1),
    }
}

/// Parsed `aterm ai review` arguments.
#[derive(Debug, Default)]
struct ReviewArgs {
    with: Option<Vec<String>>,
    all: bool,
    from: Option<String>,
    base: Option<Base>,
    timeout: Option<u32>,
    permissions: Option<Choice>,
    json: bool,
    show: Option<String>,
    words: Option<String>,
    words_stdin: bool,
}

fn parse_review(args: &[String]) -> Result<ReviewArgs, String> {
    let mut r = ReviewArgs::default();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    let mut words: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--with" => r.with = Some(csv(&value(&mut i, "--with")?)),
            "--all" => r.all = true,
            "--from" => r.from = Some(value(&mut i, "--from")?),
            "--base" => r.base = Some(Base::Ref(value(&mut i, "--base")?)),
            "--uncommitted" => r.base = Some(Base::Uncommitted),
            "--timeout" => {
                let v = value(&mut i, "--timeout")?;
                r.timeout = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (config::TIMEOUT_MIN_S..=config::TIMEOUT_MAX_S).contains(n))
                        .ok_or_else(|| {
                            format!(
                                "--timeout takes {} to {} seconds",
                                config::TIMEOUT_MIN_S,
                                config::TIMEOUT_MAX_S
                            )
                        })?,
                );
            }
            "--permissions" => {
                let v = value(&mut i, "--permissions")?;
                r.permissions = Some(
                    Choice::parse(&v)
                        .ok_or("--permissions takes inherit, yolo, edits, ask or plan")?,
                );
            }
            "--json" => r.json = true,
            "--show" => r.show = Some(value(&mut i, "--show")?),
            "-" => r.words_stdin = true,
            "--" => {
                words.extend(args[i + 1..].iter().cloned());
                break;
            }
            a if a.starts_with('-') => {
                return Err(format!(
                    "`{}` is not a flag of `aterm ai review`",
                    crate::harness::one_line(a, 40)
                ));
            }
            a => words.push(a.to_string()),
        }
        i += 1;
    }
    if r.with.is_some() && r.all {
        return Err("--with and --all are one or the other".to_string());
    }
    if !words.is_empty() {
        r.words = Some(words.join(" "));
    }
    Ok(r)
}

fn review_verb(
    env: &Env,
    runner: &dyn Runner,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm ai review: {why}");
        ExitCode::from(2)
    };
    let a = match parse_review(args) {
        Ok(a) => a,
        Err(why) => return refuse(err, &why),
    };
    let Some(store) = env.store() else {
        return refuse(err, "aterm's state directory is unknown");
    };
    if let Some(id) = &a.show {
        let Some(json) = review::kept(&store, id) else {
            return refuse(
                err,
                &format!(
                    "no kept review `{}` (reviews are kept a day)",
                    crate::harness::one_line(id, 40)
                ),
            );
        };
        if a.json {
            let _ = writeln!(out, "{json}");
        } else if let Some((id, dir, answers)) = review::answers_of(&json) {
            review::render(out, &id, &dir, &answers);
        }
        return ExitCode::SUCCESS;
    }
    if env.var(review::REVIEW_ENV).is_some_and(|v| !v.is_empty()) {
        return refuse(err, "a reviewer may not start reviews");
    }
    if env.var("CODEX_SANDBOX").is_some_and(|v| !v.is_empty()) {
        return refuse(
            err,
            "blocked by Codex's sandbox (needs network): run `aterm ai review …` outside the sandbox",
        );
    }
    let l = look(env);
    let s = &l.settings;
    if !s.reviews.0 {
        return refuse(
            err,
            &format!(
                "reviews are off (reviews = false, set in {})",
                s.reviews.1.word()
            ),
        );
    }
    let asker: &'static str = match &a.from {
        Some(f) => match connector::connector(f) {
            Some(c) => c.id,
            None => {
                return refuse(
                    err,
                    &format!("--from `{}` is no AI CLI", crate::harness::one_line(f, 40)),
                );
            }
        },
        None => asker_id(env, s),
    };
    let asked: Option<Vec<&'static str>> = if a.all {
        Some(s.reviewers.0.iter().map(|c| c.id).collect())
    } else if let Some(with) = &a.with {
        let mut ids = Vec::new();
        for w in with {
            match connector::connector(w) {
                Some(c) => ids.push(c.id),
                None => {
                    return refuse(
                        err,
                        &format!(
                            "`{}` is no AI CLI ({})",
                            crate::harness::one_line(w, 40),
                            connector::ids().join(", ")
                        ),
                    );
                }
            }
        }
        Some(ids)
    } else {
        None
    };
    let pool: Vec<&'static Connector> = match &asked {
        Some(ids) => ids
            .iter()
            .filter_map(|id| connector::connector(id))
            .collect(),
        None => s
            .reviewers
            .0
            .iter()
            .copied()
            .filter(|c| asked_by_default(s, &l.rows, c.id))
            .collect(),
    };
    let permission = match a.permissions {
        Some(Choice::Fixed(level)) => Ok((level, "--permissions".to_string())),
        Some(Choice::Inherit) => inherited(env.asker),
        None => review_level(s, env.asker),
    };
    let (level, level_why) = match permission {
        Ok(level) => level,
        Err(why) => return refuse(err, why),
    };
    let words = if a.words_stdin {
        let mut buf = Vec::new();
        let _ = io::stdin().take(WORDS_MAX as u64 + 1).read_to_end(&mut buf);
        if buf.len() > WORDS_MAX {
            return refuse(
                err,
                &format!("what to look for is over {} KiB", WORDS_MAX / 1024),
            );
        }
        match String::from_utf8(buf) {
            Ok(text) => Some(text),
            Err(_) => return refuse(err, "what to look for is not UTF-8 text"),
        }
    } else {
        a.words.clone()
    };
    let var = |k: &str| env.var(k);
    let change = match change::collect(
        runner,
        &var,
        &env.cwd,
        a.base.as_ref().unwrap_or(&Base::Branch),
        env.home.as_deref(),
    ) {
        Ok(c) => c,
        Err(change::Refused(why)) => return refuse(err, &why),
    };
    let cands = review::candidates(&pool, asked.as_deref(), asker, &l.rows, env.now);
    let count = if asked.is_some() {
        cands.len()
    } else {
        usize::from(s.review_count.0)
    };
    let budget = Duration::from_secs(u64::from(a.timeout.unwrap_or(s.review_timeout_s.0)));
    let id = review::new_id();
    let now = || crate::supervise::limit::unix_now();
    let host = review::Host {
        runner,
        var: &var,
        vars: &env.vars,
        store: &store,
        now: &now,
    };
    let dir = review::reviews_dir(&store).join(&id);
    let _ = writeln!(
        err,
        "aterm ai review {id}: {} — asking {} at {} ({level_why})",
        change.describes,
        {
            let names: Vec<&str> = cands
                .iter()
                .filter(|(_, s)| s.is_none())
                .map(|(r, _)| r.connector.id)
                .collect();
            if names.is_empty() {
                "nobody usable".to_string()
            } else {
                names.join(", ")
            }
        },
        level.word()
    );
    let ask = review::Ask {
        change: &change,
        asker,
        words: words.as_deref(),
        count,
        budget,
        level,
    };
    let answers = review::run_review(&host, &cands, &l.facts, &ask);
    let json = review::json(&id, &dir, asker, &change, &answers, env.now);
    if let Err(e) = review::keep(&store, &id, &json) {
        let _ = writeln!(err, "aterm ai review: the answers could not be kept: {e}");
    }
    if a.json {
        let _ = writeln!(out, "{json}");
    } else {
        review::render(out, &id, &dir, &answers);
    }
    if answers.iter().any(|x| x.verdict == review::Verdict::Served) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

const SECRET_MAX: u64 = 4096;

fn secret_of(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file =
        std::fs::File::open(path).map_err(|e| crate::harness::one_line(&e.to_string(), 120))?;
    let meta = file
        .metadata()
        .map_err(|e| crate::harness::one_line(&e.to_string(), 120))?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("its group or others have access to it: `chmod 600` it first".to_string());
        }
    }
    let mut raw = Vec::new();
    file.take(SECRET_MAX + 1)
        .read_to_end(&mut raw)
        .map_err(|e| crate::harness::one_line(&e.to_string(), 120))?;
    if raw.len() as u64 > SECRET_MAX {
        return Err("too large to be a secret key".to_string());
    }
    let text = std::str::from_utf8(&raw).ok().map(str::trim);
    Ok(text
        .and_then(|t| aterm_codec::base64::decode_strict(t.as_bytes()).ok())
        .unwrap_or(raw))
}

fn doc_of(path: &Path) -> Result<Vec<u8>, String> {
    crate::harness::accounts::read_bounded(path, config::FLEET_MAX_BYTES)
        .map_err(|e| crate::harness::one_line(&e.to_string(), 120))?
        .map(String::into_bytes)
        .ok_or_else(|| "no such file".to_string())
}

fn sig_path(doc: &Path) -> PathBuf {
    let mut p = doc.as_os_str().to_owned();
    p.push(".sig");
    PathBuf::from(p)
}

fn fleet_status(env: &Env, out: &mut dyn Write) -> ExitCode {
    let l = look(env);
    let s = &l.settings;
    let _ = writeln!(
        out,
        "{}{}",
        pad("source:", LABEL_W),
        s.fleet_source
            .as_deref()
            .unwrap_or("none (no [ai] fleet or ai-fleet.toml beside aterm.toml)")
    );
    let _ = writeln!(
        out,
        "{}{}",
        pad("signer:", LABEL_W),
        s.fleet_key
            .as_deref()
            .map_or("none pinned ([ai] fleet_key)".to_string(), |k| k
                .to_string())
    );
    let state = match &s.fleet {
        FleetState::None => "none".to_string(),
        FleetState::Read { row, .. } => format!(
            "read{}",
            match (&s.machine, row) {
                (Some(m), true) => format!(", with a [machine.{m}] row"),
                (Some(m), false) => format!(", with no [machine.{m}] row: its top level applies"),
                (None, _) => ", top level only (no [ai] machine is set)".to_string(),
            }
        ),
        FleetState::Refused { why, .. } => format!("refused: {why}"),
    };
    let _ = writeln!(out, "{}{state}", pad("document:", LABEL_W));
    if let Some(h) = &l.held {
        let at = |t: Option<i64>| t.map_or("never".to_string(), state::when);
        let _ = writeln!(
            out,
            "{}seq {} · fetched {} · tried {}{}",
            pad("held:", LABEL_W),
            h.seq.map_or("?".to_string(), |n| n.to_string()),
            at(h.fetched_at),
            at(h.tried_at),
            h.valid_until.map_or(String::new(), |t| format!(
                " · valid until {}",
                state::when(t)
            ))
        );
        if let Some(e) = &h.error {
            let _ = writeln!(out, "{}{e}", pad("last try:", LABEL_W));
        }
    } else if s.fleet_source.as_deref().is_some_and(fleet::is_url) {
        let _ = writeln!(
            out,
            "{}nothing yet: `aterm ai fleet fetch`",
            pad("held:", LABEL_W)
        );
    }
    for note in &s.notes {
        let _ = writeln!(
            out,
            "{}{}",
            pad("note:", LABEL_W),
            crate::harness::one_line(note, 300)
        );
    }
    ExitCode::SUCCESS
}

fn fleet_keygen(path: &Path, out: &mut dyn Write) -> Result<(), String> {
    use std::io::Write as _;
    let (secret, public) = fleet::keygen()?;
    let text = aterm_codec::base64::encode(&secret).map_err(|_| "the key does not encode")?;
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = open.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} already exists; it is never overwritten", path.display())
        } else {
            crate::harness::one_line(&e.to_string(), 120)
        }
    })?;
    file.write_all(format!("{text}\n").as_bytes())
        .and_then(|()| crate::durable::sync_all(&file))
        .map_err(|e| crate::harness::one_line(&e.to_string(), 120))?;
    let _ = writeln!(
        out,
        "secret key: {} (keep private)\n\
         public key: {public}\n\n\
         Sign with `aterm ai fleet sign <doc> --key {}`, serve <doc> and <doc>.sig over HTTPS,\n\
         and set `[ai] fleet = \"https://\u{2026}/ai-fleet.toml\"` and `fleet_key = \"{public}\"` in aterm.toml.",
        path.display(),
        path.display()
    );
    Ok(())
}

fn fleet_sign(doc: &Path, key: &Path, out: &mut dyn Write) -> Result<(), String> {
    let bytes = doc_of(doc)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "the document is not UTF-8")?;
    let (_, notes) =
        config::read_fleet(text).map_err(|why| format!("the document is refused: {why}"))?;
    let (seq, _) = fleet::meta(text)?;
    let secret = secret_of(key).map_err(|why| format!("{}: {why}", key.display()))?;
    let sig = fleet::sign(&secret, &bytes)?;
    let public = fleet::public_of(&secret)?;
    let to = sig_path(doc);
    crate::harness::upgrade_models::write_atomic_ordered(&to, &format!("{sig}\n"))
        .map_err(|e| crate::harness::one_line(&e.to_string(), 120))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o644));
    }
    let _ = writeln!(
        out,
        "signed {} → {} (signer {}…)",
        doc.display(),
        to.display(),
        fleet::fingerprint(&public)
    );
    match seq {
        Some(n) => {
            let _ = writeln!(out, "seq {n}: a machine holding a higher seq refuses it");
        }
        None => {
            let _ = writeln!(out, "warning: no `seq`; URL fetches require `seq = <n>`");
        }
    }
    for n in notes {
        let _ = writeln!(out, "note: {}", crate::harness::one_line(&n, 300));
    }
    Ok(())
}

fn fleet_verify(
    env: &Env,
    doc: &Path,
    key: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let from_flag = key.is_some();
    let key = match key {
        Some(k) => k.to_string(),
        None => look(env)
            .settings
            .fleet_key
            .ok_or("no key: pass --key <public-key>, or pin one in [ai] fleet_key")?,
    };
    let bytes = doc_of(doc)?;
    let sig = std::fs::read(sig_path(doc)).map_err(|e| {
        format!(
            "{}: {}",
            sig_path(doc).display(),
            crate::harness::one_line(&e.to_string(), 120)
        )
    })?;
    fleet::verify(&bytes, &sig, &key).map_err(|why| {
        if from_flag {
            why.replace("[ai] fleet_key", "--key")
        } else {
            why
        }
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "the document is not UTF-8")?;
    config::read_fleet(text).map_err(|why| format!("verified, but refused: {why}"))?;
    let (seq, until) = fleet::meta(text)?;
    let _ = writeln!(
        out,
        "verified under {}… · seq {}{}",
        fleet::fingerprint(&key),
        seq.map_or("none".to_string(), |n| n.to_string()),
        until.map_or(String::new(), |t| format!(
            " · valid until {}",
            state::when(t)
        ))
    );
    Ok(())
}

fn fleet_verb(
    env: &Env,
    fetcher: &dyn Fetcher,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let fail = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(
            err,
            "aterm ai fleet: {}",
            crate::harness::one_line(why, 300)
        );
        ExitCode::from(1)
    };
    let usage = |err: &mut dyn Write| {
        let _ = writeln!(
            err,
            "usage: aterm ai fleet [status | fetch | keygen <secret-key-file> | \
             sign <doc> --key <secret-key-file> | verify <doc> [--key <public-key>]]"
        );
        ExitCode::from(2)
    };
    let arg = |i: usize| args.get(i).map(String::as_str);
    match (arg(0), arg(1), arg(2), arg(3), args.len()) {
        (None | Some("status"), _, _, _, 0 | 1) => fleet_status(env, out),
        (Some("fetch"), _, _, _, 1) => match fleet_fetch(env, fetcher, true) {
            None => {
                let _ = writeln!(
                    err,
                    "aterm ai fleet fetch: [ai] fleet names no https:// URL; nothing to fetch"
                );
                ExitCode::from(2)
            }
            Some(got) => {
                let _ = writeln!(out, "{}", got.says());
                match got {
                    Fetched::Fresh(_) | Fetched::Same(_) => ExitCode::SUCCESS,
                    _ => ExitCode::from(1),
                }
            }
        },
        (Some("keygen"), Some(path), _, _, 2) => match fleet_keygen(Path::new(path), out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(why) => fail(err, &why),
        },
        (Some("sign"), Some(doc), Some("--key"), Some(key), 4) => {
            match fleet_sign(Path::new(doc), Path::new(key), out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => fail(err, &why),
            }
        }
        (Some("verify"), Some(doc), None, _, 2) => {
            match fleet_verify(env, Path::new(doc), None, out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => fail(err, &why),
            }
        }
        (Some("verify"), Some(doc), Some("--key"), Some(key), 4) => {
            match fleet_verify(env, Path::new(doc), Some(key), out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => fail(err, &why),
            }
        }
        _ => usage(err),
    }
}

/// Runs `aterm ai <args>` against `env`, `runner` and `fetcher`.
pub fn run(
    env: &Env,
    runner: &dyn Runner,
    fetcher: &dyn Fetcher,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let flag = |f: &str| args.iter().any(|a| a == f);
    match args.first().map(String::as_str) {
        None | Some("status") => {
            let l = look(env);
            if flag("--json") {
                let _ = writeln!(out, "{}", render_json(&l, env.now, &next_act(&l)));
            } else {
                render_text(out, &l, false, &next_act(&l));
            }
            ExitCode::SUCCESS
        }
        Some("--json") => {
            let l = look(env);
            let _ = writeln!(out, "{}", render_json(&l, env.now, &next_act(&l)));
            ExitCode::SUCCESS
        }
        Some("doctor") => {
            let rest = &args[1..];
            let mut on: Option<Vec<String>> = None;
            let mut fleet = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--read-only" | "--json" => {}
                    "--fleet" => fleet = true,
                    "--on" => {
                        i += 1;
                        match rest.get(i) {
                            Some(v) => on = Some(csv(v)),
                            None => {
                                let _ = writeln!(err, "aterm ai doctor: --on needs <host>,…");
                                return ExitCode::from(2);
                            }
                        }
                    }
                    other => {
                        let _ = writeln!(
                            err,
                            "aterm ai doctor: `{}` is not a flag\n\n{USAGE}",
                            crate::harness::one_line(other, 40)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            let read_only = flag("--read-only");
            let json = flag("--json");
            let _ = fleet_fetch(env, fetcher, false);
            if fleet || on.is_some() {
                doctor_away(env, runner, on.as_deref(), read_only, json, out, err)
            } else {
                doctor_here(env, runner, read_only, json, out, err)
            }
        }
        Some("login") => match args.get(1) {
            Some(id) if args.len() == 2 => login_verb(env, runner, id, out, err),
            _ => {
                let _ = writeln!(
                    err,
                    "usage: aterm ai login <cli> ({})",
                    connector::ids().join(", ")
                );
                ExitCode::from(2)
            }
        },
        Some("review") => review_verb(env, runner, &args[1..], out, err),
        Some("fleet") => fleet_verb(env, fetcher, &args[1..], out, err),
        Some("help" | "--help" | "-h") => {
            let _ = writeln!(out, "{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            let _ = writeln!(
                err,
                "aterm ai: `{}` is not a verb\n\n{USAGE}",
                crate::harness::one_line(other, 40)
            );
            ExitCode::from(2)
        }
    }
}

/// `aterm ai …`, from the front door.
#[must_use = "return or inspect the command exit status"]
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let env = Env::from_process();
    let mut out = io::stdout().lock();
    let mut err = io::stderr();
    run(&env, &LiveRunner, &LiveFetcher, &args, &mut out, &mut err)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
