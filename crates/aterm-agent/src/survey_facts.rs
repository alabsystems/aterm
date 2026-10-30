// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm survey facts (C16): host label, version, window liveness (`window` / `window_last`),
//! session count, per-row account health/enrollment/usage/last-use across pooled CLIs, pure-Rust
//! SSH host-key `SHA256:…` fingerprint, and vault-server subscriber last-heard records.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::ai::connector::Cli;
use crate::harness::accounts;
use crate::harness::cli::{Env, SurveyPool};

const WINDOW_LAST_FILE: &str = "window-last";
const WINDOW_LAST_FLUSH_SECS: u64 = 60;
const SSH_PUBKEY_MAX: usize = 16 << 10;

/// Per-account health, enrollment, usage and last-use observation carried in survey facts (§6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyAccountFact {
    /// Pooled CLI identifier (`claude`, `codex`, `grok`, `kimi`).
    pub pool: String,
    /// Account label in the roster (`accounts.toml` / `accounts-<cli>.toml`).
    pub id: String,
    /// Canonical [`accounts::Condition`] word (`usable`, `limited`, `low`, `not-enrolled`, …).
    pub condition: String,
    /// Whether the login directory is enrolled on this machine.
    pub enrolled: bool,
    /// Active 5-hour window utilization percent, when observed and still running.
    pub usage_5h_pct: Option<u32>,
    /// Active 7-day weekly utilization percent, when observed and still running.
    pub usage_7d_pct: Option<u32>,
    /// Unix timestamp (seconds) when the weekly window resets, when known.
    pub week_reset: Option<i64>,
    /// Unix timestamp (seconds) until which a standing wall blocks this account, when walled.
    pub walled_until: Option<i64>,
    /// Unix timestamp (seconds) when a session here last ran on this account.
    pub last_used: Option<i64>,
}

/// One subscriber last-heard record from a vault server running on this machine (§7.2, C16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHeardFact {
    /// Vault name (`alab`).
    pub vault: String,
    /// Subscriber `age1…` recipient.
    pub subject: String,
    /// Unix timestamp (seconds) of the last envelope GET / long-poll carrying that subscriber.
    pub at: u64,
}

/// Complete C16 survey facts for one machine, serializable onto and parseable from `survey` wire
/// replies (`OK <n>\ninstance …\n…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmSurveyFacts {
    /// Fleet node id (`node=` on `instance`), when enrolled in a fleet.
    pub node: Option<String>,
    /// Canonical machine label (`m<N>` or configured `[ai]`/`[fleet] machine` / vault host).
    pub label: Option<String>,
    /// Running `aterm` version string.
    pub version: String,
    /// Whether a GUI window instance is live right now (`1` in a window host, `0` headless).
    pub window: bool,
    /// Unix timestamp (seconds) when a window last ran here (`<state>/window-last`).
    pub window_last: Option<u64>,
    /// Live session count in this instance.
    pub sessions: usize,
    /// OpenSSH host public key fingerprint (`SHA256:<unpadded-base64>`), read in pure Rust.
    pub ssh_host_key: Option<String>,
    /// Per-row account facts across pooled CLIs, in `Cli::ALL` × roster order, each as `aterm
    /// harness accounts --json` reads it ([`crate::harness::cli::survey_accounts`]).
    pub accounts: Vec<SurveyAccountFact>,
    /// The pooled CLIs whose account records could not be read (`accounts_unread=`): a reader
    /// takes such a pool's rows as unknown, never as none.
    pub accounts_unread: Vec<String>,
    /// The row each supervised session was last read running on, by pool and session id. Not on
    /// the wire: this instance's own session rows name it.
    pub session_accounts: BTreeMap<(String, String), String>,
    /// Subscriber last-heard records when this machine serves vaults (`~/.config/aterm/secrets/served`).
    pub server_heard: Vec<ServerHeardFact>,
}

fn word_token(text: Option<&str>) -> String {
    text.filter(|t| !t.is_empty())
        .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode)
}

fn opt_u64_token(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn opt_u32_token(value: Option<u32>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn opt_i64_token(value: Option<i64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

impl SwarmSurveyFacts {
    /// Collect live survey facts from the current process environment.
    #[must_use]
    pub fn collect(sessions: usize, window: bool, node: Option<String>) -> Self {
        let now = aterm_time::unix_secs()
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or(0);
        match Env::from_process() {
            Ok(env) => Self::collect_in(&env, sessions, window, node, cached_system_ssh_host_key()),
            Err(_) => Self {
                node,
                label: None,
                version: aterm_types::version::app_version().to_owned(),
                window,
                window_last: window.then(|| u64::try_from(now.max(0)).unwrap_or(0)),
                sessions,
                ssh_host_key: cached_system_ssh_host_key(),
                accounts: Vec::new(),
                accounts_unread: Cli::ALL.iter().map(|cli| cli.id().to_owned()).collect(),
                session_accounts: BTreeMap::new(),
                server_heard: Vec::new(),
            },
        }
    }

    /// Collect survey facts rooted at an explicit `aterm_state`, `home`, and `config` path
    /// (used by `aterm node serve` and isolated tests without reading process globals).
    #[must_use]
    pub fn collect_at(
        aterm_state: &Path,
        home: Option<&Path>,
        config: Option<&Path>,
        sessions: usize,
        window: bool,
        node: Option<String>,
        now_secs: i64,
    ) -> Self {
        let env = Env {
            state: aterm_state
                .join("harness")
                .join(crate::harness::cli::HARNESS),
            aterm_state: Some(aterm_state.to_path_buf()),
            cwd: aterm_state.to_path_buf(),
            home: home.map(Path::to_path_buf),
            claude_dir: home.map(|h| h.join(".claude")),
            claude_config_unusable: None,
            claude_profile: home.map(|h| h.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            codex_home: None,
            codex_login: None,
            agy_conversation: None,
            now: now_secs,
            sid: String::new(),
            sock: None,
            config: config.map(Path::to_path_buf),
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        };
        Self::collect_in(&env, sessions, window, node, cached_system_ssh_host_key())
    }

    /// Collect survey facts under an explicit [`Env`] and pre-read `ssh_host_key` (for tests and
    /// deterministic callers).
    #[must_use]
    pub fn collect_in(
        env: &Env,
        sessions: usize,
        window: bool,
        node: Option<String>,
        ssh_host_key: Option<String>,
    ) -> Self {
        let now_u64 = u64::try_from(env.now.max(0)).unwrap_or(0);
        let window_last = touch_or_read_window_last(&env.state, window, now_u64);
        let label = resolve_machine_label(env);
        let (accounts, accounts_unread, session_accounts) = collect_account_facts(env);
        let server_heard = collect_server_heard(env);
        Self {
            node,
            label,
            version: aterm_types::version::app_version().to_owned(),
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts,
            server_heard,
        }
    }

    /// Space-prefixed `key=value` tokens appended onto `survey`'s `instance …` header line: with
    /// `accounts=` counting the `account` lines that follow, and `accounts_unread=` naming the
    /// pools none of whose rows they list because their records could not be read (`-` for none).
    #[must_use]
    pub fn header_tokens(&self) -> String {
        let headroom = live_cpu_headroom_pct()
            .map(|pct| format!(" cpu_headroom={pct}"))
            .unwrap_or_default();
        format!(
            " label={} window={} window_last={} ssh_host_key={}{headroom} accounts={} \
             accounts_unread={}",
            word_token(self.label.as_deref()),
            u8::from(self.window),
            opt_u64_token(self.window_last),
            word_token(self.ssh_host_key.as_deref()),
            self.accounts.len(),
            word_token(Some(&self.accounts_unread.join(","))),
        )
    }

    /// Extract optional `cpu_headroom=<0..=100>` from a `survey` wire reply's `instance` line.
    #[must_use]
    pub fn parse_cpu_headroom_pct(reply: &str) -> Option<u32> {
        for raw in reply.lines() {
            let line = raw.trim_end_matches('\r');
            if line.starts_with("instance ") {
                let fields = parse_fields(line);
                return field_raw(&fields, "cpu_headroom")
                    .and_then(|v| v.parse::<u32>().ok())
                    .map(|pct| pct.min(100));
            }
        }
        None
    }

    /// Additional `account …` and `server_heard …` lines emitted in a `survey` reply, each
    /// account's `id=` masked where it is an email ([`aterm_ctl::masked_label`]), so none crosses
    /// the fleet.
    #[must_use]
    pub fn extra_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.accounts.len() + self.server_heard.len());
        for acc in &self.accounts {
            lines.push(format!(
                "account pool={} id={} condition={} enrolled={} pct5={} pct7={} \
                 week_reset={} walled_until={} last_used={}",
                aterm_codec::pct::encode(&acc.pool),
                aterm_codec::pct::encode(&aterm_ctl::masked_label(&acc.id)),
                aterm_codec::pct::encode(&acc.condition),
                u8::from(acc.enrolled),
                opt_u32_token(acc.usage_5h_pct),
                opt_u32_token(acc.usage_7d_pct),
                opt_i64_token(acc.week_reset),
                opt_i64_token(acc.walled_until),
                opt_i64_token(acc.last_used),
            ));
        }
        for heard in &self.server_heard {
            lines.push(format!(
                "server_heard vault={} subject={} at={}",
                aterm_codec::pct::encode(&heard.vault),
                aterm_codec::pct::encode(&heard.subject),
                heard.at,
            ));
        }
        lines
    }

    /// Render a standalone `OK <n>\ninstance …\n` survey-facts wire payload (used by node-job
    /// heartbeats when no window socket is needed and by round-trip tests).
    #[must_use]
    pub fn to_survey_reply(
        &self,
        pid: u32,
        host: Option<&str>,
        at_ms: u64,
        instances: usize,
        drive: bool,
    ) -> String {
        let extra = self.extra_lines();
        let count = 1 + extra.len();
        let host = word_token(host);
        let version = aterm_codec::pct::encode(&self.version);
        let node = word_token(self.node.as_deref());
        let drive_tok = if drive { " drive=1" } else { "" };
        let mut out = format!(
            "OK {count}\ninstance pid={pid} host={host} version={version} sessions={} \
             level=facts cut=0 at_ms={at_ms} node={node} instances={instances}{}{drive_tok}\n",
            self.sessions,
            self.header_tokens(),
        );
        for line in extra {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// Parse and validate [`SwarmSurveyFacts`] from a `survey` wire reply.
    ///
    /// # Errors
    /// Returns why the reply is not a valid C16 survey-facts payload (missing `instance` header,
    /// missing required C16 header field, or malformed `account` / `server_heard` record).
    pub fn from_survey_reply(reply: &str) -> Result<Self, String> {
        let mut instance: Option<Vec<(String, String)>> = None;
        let mut accounts = Vec::new();
        let mut server_heard = Vec::new();

        for raw in reply.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with("OK ") || line.starts_with("session ") {
                continue;
            }
            if line.starts_with("instance ") {
                instance = Some(parse_fields(line));
            } else if line.starts_with("account ") {
                accounts.push(parse_account_line(&parse_fields(line))?);
            } else if line.starts_with("server_heard ") {
                server_heard.push(parse_server_heard_line(&parse_fields(line))?);
            }
        }

        let header =
            instance.ok_or_else(|| "missing instance header in survey reply".to_owned())?;
        let version = required_nonempty(&header, "version")?;
        let sessions = required_raw(&header, "sessions")?
            .parse::<usize>()
            .map_err(|_| "invalid sessions field in instance header".to_owned())?;
        let window = parse_bool01(required_raw(&header, "window")?, "window")?;
        let window_last = parse_opt_u64(required_raw(&header, "window_last")?, "window_last")?;
        let label = optional_word(required_raw(&header, "label")?);
        let ssh_host_key = optional_word(required_raw(&header, "ssh_host_key")?);
        let node = field_raw(&header, "node").and_then(optional_word);
        // Optional: an aterm whose C16 facts predate it names no unread pool.
        let accounts_unread = field_raw(&header, "accounts_unread")
            .and_then(optional_word)
            .map_or_else(Vec::new, |pools| {
                pools.split(',').map(str::to_owned).collect()
            });

        Ok(Self {
            node,
            label,
            version,
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts: BTreeMap::new(),
            server_heard,
        })
    }
}

fn parse_fields(line: &str) -> Vec<(String, String)> {
    line.split(' ')
        .skip(1)
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
        .collect()
}

fn field_raw<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn required_raw<'a>(fields: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    field_raw(fields, key).ok_or_else(|| format!("missing `{key}` field"))
}

fn required_nonempty(fields: &[(String, String)], key: &str) -> Result<String, String> {
    let raw = required_raw(fields, key)?;
    if raw.is_empty() || raw == "-" {
        return Err(format!("empty `{key}` field"));
    }
    Ok(raw.to_owned())
}

fn optional_word(raw: &str) -> Option<String> {
    (!raw.is_empty() && raw != "-").then(|| raw.to_owned())
}

fn parse_bool01(raw: &str, key: &str) -> Result<bool, String> {
    match raw {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(format!("invalid `{key}` field: expected 0 or 1")),
    }
}

fn parse_opt_u64(raw: &str, key: &str) -> Result<Option<u64>, String> {
    if raw == "-" {
        return Ok(None);
    }
    raw.parse::<u64>()
        .map(Some)
        .map_err(|_| format!("invalid `{key}` field"))
}

fn parse_opt_u32(raw: &str, key: &str) -> Result<Option<u32>, String> {
    if raw == "-" {
        return Ok(None);
    }
    raw.parse::<u32>()
        .map(Some)
        .map_err(|_| format!("invalid `{key}` field"))
}

fn parse_opt_i64(raw: &str, key: &str) -> Result<Option<i64>, String> {
    if raw == "-" {
        return Ok(None);
    }
    raw.parse::<i64>()
        .map(Some)
        .map_err(|_| format!("invalid `{key}` field"))
}

fn parse_account_line(fields: &[(String, String)]) -> Result<SurveyAccountFact, String> {
    Ok(SurveyAccountFact {
        pool: required_nonempty(fields, "pool")?,
        id: required_nonempty(fields, "id")?,
        condition: required_nonempty(fields, "condition")?,
        enrolled: parse_bool01(required_raw(fields, "enrolled")?, "enrolled")?,
        usage_5h_pct: parse_opt_u32(required_raw(fields, "pct5")?, "pct5")?,
        usage_7d_pct: parse_opt_u32(required_raw(fields, "pct7")?, "pct7")?,
        week_reset: parse_opt_i64(required_raw(fields, "week_reset")?, "week_reset")?,
        walled_until: parse_opt_i64(required_raw(fields, "walled_until")?, "walled_until")?,
        last_used: parse_opt_i64(required_raw(fields, "last_used")?, "last_used")?,
    })
}

fn parse_server_heard_line(fields: &[(String, String)]) -> Result<ServerHeardFact, String> {
    let vault = required_nonempty(fields, "vault")?;
    let subject = required_nonempty(fields, "subject")?;
    let at = required_raw(fields, "at")?
        .parse::<u64>()
        .map_err(|_| "invalid `at` field on server_heard".to_owned())?;
    Ok(ServerHeardFact { vault, subject, at })
}

fn touch_or_read_window_last(state_dir: &Path, window: bool, now: u64) -> Option<u64> {
    static LAST_FLUSHED: Mutex<Option<(std::path::PathBuf, u64)>> = Mutex::new(None);
    let path = state_dir.join(WINDOW_LAST_FILE);
    let stored = crate::harness::accounts::read_bounded(&path, 256)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<u64>().ok());
    if !window {
        return stored;
    }
    let effective = stored.map_or(now, |prev| prev.max(now));
    let flush_due = {
        let guard = LAST_FLUSHED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_ref() {
            Some((dir, last))
                if dir == state_dir && effective.saturating_sub(*last) < WINDOW_LAST_FLUSH_SECS =>
            {
                stored.is_none()
            }
            _ => stored.is_none_or(|prev| effective.saturating_sub(prev) >= WINDOW_LAST_FLUSH_SECS),
        }
    };
    if flush_due
        && crate::harness::upgrade_models::write_atomic(&path, &format!("{effective}\n")).is_ok()
    {
        let mut guard = LAST_FLUSHED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some((state_dir.to_path_buf(), effective));
    }
    Some(effective)
}

#[cfg(unix)]
fn local_roster_denial(home: Option<&Path>) -> (Vec<String>, bool) {
    let Some(home) = home else {
        return (Vec::new(), false);
    };
    let paths = aterm_secrets::client::Paths::for_home(home);
    let recipient_denied = aterm_secrets::client::local_recipient_denied_in_roster(&paths);
    let Ok(Some(roster)) = aterm_secrets::client::swarm_roster(&paths) else {
        return (Vec::new(), recipient_denied);
    };
    (roster.deny_labels, recipient_denied)
}

#[cfg(not(unix))]
fn local_roster_denial(_home: Option<&Path>) -> (Vec<String>, bool) {
    (Vec::new(), false)
}

fn resolve_machine_label(env: &Env) -> Option<String> {
    let (deny_labels, recipient_denied) = local_roster_denial(env.home.as_deref());
    if recipient_denied {
        return None;
    }
    let is_denied_label = |candidate: &str| {
        #[cfg(unix)]
        let canon = aterm_secrets::swarm_roster::leading_m_label(candidate)
            .map(|(_, c)| c)
            .unwrap_or_else(|| candidate.trim().to_ascii_lowercase());
        #[cfg(not(unix))]
        let canon = candidate.trim().to_ascii_lowercase();
        deny_labels.iter().any(|d| d.eq_ignore_ascii_case(&canon))
    };
    if let Some(config_path) = env.config.as_deref()
        && let Ok(Some(text)) = crate::harness::accounts::read_bounded(config_path, 1024 * 1024)
        && let Ok(table) = aterm_toml::from_str::<aterm_toml::Value>(&text)
    {
        for (section, key) in [
            ("swarm", "label"),
            ("ai", "machine"),
            ("fleet", "machine"),
            ("harness", "machine"),
        ] {
            if let Some(machine) = table
                .get(section)
                .and_then(aterm_toml::Value::as_table)
                .and_then(|t| t.get(key))
                .and_then(aterm_toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && !is_denied_label(s))
            {
                return Some(machine.to_owned());
            }
        }
    }
    resolve_vault_host_label(env).filter(|h| !is_denied_label(h))
}

#[cfg(unix)]
fn resolve_vault_host_label(env: &Env) -> Option<String> {
    let home = env.home.as_deref()?;
    let paths = aterm_secrets::client::Paths::for_home(home);
    if let Ok(Some(label)) = aterm_secrets::client::resolve_local_swarm_label(&paths, &[]) {
        return Some(label);
    }
    if aterm_secrets::client::swarm_roster(&paths)
        .ok()
        .flatten()
        .is_some()
    {
        return None;
    }
    let status = aterm_secrets::client::status(&paths).ok()?;
    status
        .config
        .map(|cfg| cfg.host)
        .filter(|h| !h.trim().is_empty())
}

#[cfg(not(unix))]
fn resolve_vault_host_label(_env: &Env) -> Option<String> {
    None
}

/// Each pooled CLI's rows, the pools that could not be read, and the row each session runs on,
/// as `aterm harness accounts --json` reads them beside `env`'s `aterm.toml`.
fn collect_account_facts(env: &Env) -> AccountFacts {
    account_facts_of(Cli::ALL.into_iter().map(|cli| {
        let read = env
            .config
            .as_deref()
            .ok_or_else(|| "no aterm.toml path is known".to_owned())
            .and_then(|config| {
                crate::harness::cli::survey_accounts(
                    cli,
                    (config, &env.state, env.home.as_deref()),
                    env.now,
                )
            });
        (cli, read)
    }))
}

/// What [`collect_account_facts`] reads: the rows, the unread pools, and each session's row.
type AccountFacts = (
    Vec<SurveyAccountFact>,
    Vec<String>,
    BTreeMap<(String, String), String>,
);

/// The account facts each pool's read makes: its rows, or its id among the unread where it could
/// not be read (a pool left out with no word would read as one holding no row).
fn account_facts_of(
    reads: impl IntoIterator<Item = (Cli, Result<SurveyPool, String>)>,
) -> AccountFacts {
    let mut rows = Vec::new();
    let mut unread = Vec::new();
    let mut sessions = BTreeMap::new();
    for (cli, read) in reads {
        let pool = cli.id().to_owned();
        let Ok(read) = read else {
            unread.push(pool);
            continue;
        };
        rows.extend(read.rows.into_iter().map(|row| SurveyAccountFact {
            pool: pool.clone(),
            walled_until: match row.condition {
                accounts::Condition::Limited { until } => Some(until),
                _ => None,
            },
            id: row.label,
            condition: row.condition.as_str().to_owned(),
            enrolled: row.enrolled,
            usage_5h_pct: row.five_hour_pct,
            usage_7d_pct: row.seven_day_pct,
            week_reset: row.week_reset,
            last_used: row.used_at,
        }));
        sessions.extend(
            read.sessions
                .into_iter()
                .map(|(sid, label)| ((pool.clone(), sid), label)),
        );
    }
    (rows, unread, sessions)
}

#[cfg(unix)]
fn collect_server_heard(env: &Env) -> Vec<ServerHeardFact> {
    let Some(home) = env.home.as_deref() else {
        return Vec::new();
    };
    let served = aterm_secrets::client::Paths::for_home(home).served();
    aterm_secrets::server::read_last_heard(&served)
        .into_iter()
        .map(|(vault, subject, at)| ServerHeardFact { vault, subject, at })
        .collect()
}

#[cfg(not(unix))]
fn collect_server_heard(_env: &Env) -> Vec<ServerHeardFact> {
    Vec::new()
}

fn cached_system_ssh_host_key() -> Option<String> {
    static HOST_KEY: OnceLock<Option<String>> = OnceLock::new();
    HOST_KEY
        .get_or_init(|| {
            #[cfg(unix)]
            {
                ssh_host_key_fingerprint_in(Path::new("/etc/ssh"))
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
        .clone()
}

/// Compute the OpenSSH `SHA256:<unpadded-base64>` host key fingerprint from `dir`
/// (`ssh_host_ed25519_key.pub`, else `ecdsa`, else `rsa`) in pure Rust without spawning
/// `ssh-keygen`.
#[must_use]
pub fn ssh_host_key_fingerprint_in(dir: &Path) -> Option<String> {
    for file in [
        "ssh_host_ed25519_key.pub",
        "ssh_host_ecdsa_key.pub",
        "ssh_host_rsa_key.pub",
    ] {
        let path = dir.join(file);
        let Ok(Some(text)) = crate::harness::accounts::read_bounded(&path, SSH_PUBKEY_MAX as u64)
        else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        if let Some(fp) = parse_openssh_pubkey_sha256(&text) {
            return Some(fp);
        }
    }
    None
}

fn parse_openssh_pubkey_sha256(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let mut parts = line.split_ascii_whitespace();
    let key_type = parts.next()?;
    let b64_blob = parts.next()?;
    if !key_type.starts_with("ssh-") && !key_type.starts_with("ecdsa-") {
        return None;
    }
    let decoded = aterm_codec::base64::decode(b64_blob).ok()?;
    if decoded.len() < 16 {
        return None;
    }
    let digest = aterm_digest::Sha256::digest(&decoded);
    let encoded = aterm_codec::base64::encode(&digest).ok()?;
    Some(format!("SHA256:{}", encoded.trim_end_matches('=')))
}

#[cfg(unix)]
fn live_cpu_headroom_pct() -> Option<u32> {
    let cores = std::thread::available_parallelism().ok()?.get();
    if cores == 0 {
        return None;
    }
    unsafe extern "C" {
        fn getloadavg(loadavg: *mut f64, nelem: libc::c_int) -> libc::c_int;
    }
    let mut load = [0.0f64; 1];
    // SAFETY: `load.as_mut_ptr()` points to a valid initialized 1-element `f64` array and we pass
    // `1` as the element count to POSIX `getloadavg`.
    let n = unsafe { getloadavg(load.as_mut_ptr(), 1) };
    if n < 1 || !load[0].is_finite() || load[0] < 0.0 {
        return None;
    }
    let busy_ratio = (load[0] / (cores as f64)).clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let headroom = ((1.0 - busy_ratio) * 100.0).round() as u32;
    Some(headroom.min(100))
}

#[cfg(not(unix))]
fn live_cpu_headroom_pct() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_facts() -> SwarmSurveyFacts {
        SwarmSurveyFacts {
            node: Some("f1e2d3c4".to_owned()),
            label: Some("m22".to_owned()),
            version: "0.146.0".to_owned(),
            window: true,
            window_last: Some(1_793_160_000),
            sessions: 4,
            ssh_host_key: Some("SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU".to_owned()),
            accounts: vec![
                SurveyAccountFact {
                    pool: "claude".to_owned(),
                    id: "c01".to_owned(),
                    condition: "usable".to_owned(),
                    enrolled: true,
                    usage_5h_pct: Some(34),
                    usage_7d_pct: Some(61),
                    week_reset: Some(1_793_500_000),
                    walled_until: None,
                    last_used: Some(1_793_159_900),
                },
                SurveyAccountFact {
                    pool: "codex".to_owned(),
                    id: "x02".to_owned(),
                    condition: "limited".to_owned(),
                    enrolled: true,
                    usage_5h_pct: Some(100),
                    usage_7d_pct: Some(88),
                    week_reset: Some(1_793_600_000),
                    walled_until: Some(1_793_170_000),
                    last_used: Some(1_793_158_000),
                },
                SurveyAccountFact {
                    pool: "claude".to_owned(),
                    id: "c03".to_owned(),
                    condition: "not-enrolled".to_owned(),
                    enrolled: false,
                    usage_5h_pct: None,
                    usage_7d_pct: None,
                    week_reset: None,
                    walled_until: None,
                    last_used: None,
                },
            ],
            accounts_unread: vec!["grok".to_owned(), "kimi".to_owned()],
            session_accounts: BTreeMap::new(),
            server_heard: vec![ServerHeardFact {
                vault: "alab".to_owned(),
                subject: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p"
                    .to_owned(),
                at: 1_793_159_995,
            }],
        }
    }

    #[test]
    fn facts_round_trip_and_missing_field_negative_controls() {
        let expected = sample_facts();
        let wire = expected.to_survey_reply(4242, Some("m22.local"), 1_793_160_000_000, 1, false);
        let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid C16 wire reply");
        assert_eq!(parsed, expected);

        // Negative controls: omitting any required C16 field on `instance`, `account`, or
        // `server_heard` fails validation instead of silently inventing defaults.
        for missing_token in [
            " window=1",
            " window_last=1793160000",
            " label=m22",
            " ssh_host_key=SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
            " pct5=34",
            " last_used=1793159900",
            " at=1793159995",
        ] {
            let broken = wire.replacen(missing_token, "", 1);
            assert_ne!(broken, wire, "token {missing_token} must appear in wire");
            assert!(
                SwarmSurveyFacts::from_survey_reply(&broken).is_err(),
                "removing `{missing_token}` must be rejected: {broken}"
            );
        }
    }

    /// The header counts the account lines and names the pools that could not be read, which an
    /// aterm whose facts predate the field leaves out; no email crosses the wire.
    #[test]
    fn the_header_counts_the_rows_and_names_each_unread_pool() {
        let facts = sample_facts();
        let wire = facts.to_survey_reply(4242, Some("m22.local"), 1_793_160_000_000, 1, false);
        let head = wire.lines().nth(1).expect("the instance line");
        assert!(head.contains(" accounts=3 "), "{head}");
        let unread = |wire: &str| {
            SwarmSurveyFacts::from_survey_reply(wire)
                .expect("valid C16 wire reply")
                .accounts_unread
        };
        assert_eq!(unread(&wire), ["grok", "kimi"]);
        let none = SwarmSurveyFacts {
            accounts_unread: Vec::new(),
            ..facts.clone()
        };
        let wire = none.to_survey_reply(4242, None, 0, 1, false);
        assert!(wire.contains(" accounts_unread=-\n"), "{wire}");
        assert!(unread(&wire).is_empty());
        assert!(unread(&wire.replacen(" accounts_unread=-", "", 1)).is_empty());

        let mut emailed = facts;
        emailed.accounts[0].id = "abcdef@example.com".to_owned();
        let wire = emailed.to_survey_reply(4242, None, 0, 1, false);
        assert!(
            !wire.contains("example") && !wire.contains("abcdef"),
            "no email on the wire: {wire}"
        );
        assert!(
            wire.contains(" id=ab%E2%80%A6@%E2%80%A6 "),
            "masked: {wire}"
        );
    }

    /// Each pool's read makes its rows, or names it unread: a pool that could not be read never
    /// reads as one holding no row, and the others' rows still count.
    #[test]
    fn a_pool_that_could_not_be_read_is_named_and_the_others_still_list() {
        use crate::harness::accounts::{Class, Condition};
        use crate::harness::cli::SurveyAccount;
        let row = |label: &str, condition: Condition| SurveyAccount {
            label: label.to_owned(),
            class: Class::Dedicated,
            enrolled: condition != Condition::NotEnrolled,
            condition,
            five_hour_pct: Some(10),
            seven_day_pct: Some(42),
            week_reset: Some(1_793_500_000),
            used_at: Some(1_793_130_000),
        };
        let claude = SurveyPool {
            rows: vec![
                row("main", Condition::Usable),
                row(
                    "work-2",
                    Condition::Limited {
                        until: 1_793_140_000,
                    },
                ),
            ],
            sessions: BTreeMap::from([("s-a1".to_owned(), "work-2".to_owned())]),
        };
        let (rows, unread, sessions) = account_facts_of([
            (Cli::Claude, Ok(claude)),
            (Cli::Codex, Err("its roster could not be read".to_owned())),
            (Cli::Grok, Ok(SurveyPool::default())),
        ]);
        assert_eq!(unread, ["codex"]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.pool.as_str(), row.id.as_str()))
                .collect::<Vec<_>>(),
            [("claude", "main"), ("claude", "work-2")]
        );
        assert_eq!(
            (rows[0].usage_7d_pct, rows[0].week_reset, rows[0].last_used),
            (Some(42), Some(1_793_500_000), Some(1_793_130_000))
        );
        assert_eq!(
            [rows[0].walled_until, rows[1].walled_until],
            [None, Some(1_793_140_000)]
        );
        assert_eq!(rows[1].condition, "limited");
        assert_eq!(
            sessions.get(&("claude".to_owned(), "s-a1".to_owned())),
            Some(&"work-2".to_owned())
        );
    }

    #[test]
    fn ssh_host_key_fingerprint_reads_ed25519_in_pure_rust() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        assert_eq!(ssh_host_key_fingerprint_in(dir.path()), None);

        let blob = [0x42u8; 51];
        let b64 = aterm_codec::base64::encode(&blob).expect("b64");
        std::fs::write(
            dir.path().join("ssh_host_ed25519_key.pub"),
            format!("ssh-ed25519 {b64} root@m22\n"),
        )
        .expect("write pubkey");

        let expected_digest = aterm_digest::Sha256::digest(blob);
        let expected_fp = format!(
            "SHA256:{}",
            aterm_codec::base64::encode(&expected_digest)
                .expect("b64")
                .trim_end_matches('=')
        );
        assert_eq!(ssh_host_key_fingerprint_in(dir.path()), Some(expected_fp));

        // Negative control: corrupt base64 or non-SSH prefix is rejected.
        let bad_dir = aterm_tempfile::tempdir().expect("tempdir");
        std::fs::write(
            bad_dir.path().join("ssh_host_ed25519_key.pub"),
            "not-ssh $$$invalid$$$\n",
        )
        .expect("write corrupt");
        assert_eq!(ssh_host_key_fingerprint_in(bad_dir.path()), None);
    }

    #[test]
    fn window_last_persists_from_windowed_to_headless_collect() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        assert_eq!(
            touch_or_read_window_last(&state, false, 1_793_160_000),
            None
        );
        assert_eq!(
            touch_or_read_window_last(&state, true, 1_793_160_000),
            Some(1_793_160_000)
        );
        assert_eq!(
            touch_or_read_window_last(&state, false, 1_793_160_300),
            Some(1_793_160_000)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_machine_label_enforces_roster_deny_lists_and_rejects_non_canonical_vault_host() {
        use aterm_secrets::swarm_roster::{ROSTER_ENTRY, SwarmMember, SwarmRoster};

        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let state = dir.path().join("state");
        let config = dir.path().join("aterm.toml");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let srv_dir = aterm_tempfile::tempdir().expect("server dir");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: srv_dir.path().to_path_buf(),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .expect("start server");
        let url = format!("http://{}/v1/test", running.addr);
        let paths = aterm_secrets::client::Paths::for_home(&home);
        aterm_secrets::client::create(&paths, &url, None, Some("mac-mini-m4"))
            .expect("create vault");
        let rec = aterm_secrets::client::machine_identity(&paths)
            .expect("identity")
            .recipient()
            .to_bech32();

        // 1. When a SwarmRoster exists and this recipient is not yet listed (and `cfg.host` is
        //    `"Mac-mini-M4"`), `resolve_vault_host_label` must NOT return `"Mac-mini-M4"`, and
        //    a denied `[swarm] label = "m4"` in `aterm.toml` must also be rejected.
        let roster = SwarmRoster {
            members: vec![SwarmMember {
                recipient: "age1other".to_owned(),
                vault_name: "m22".to_owned(),
                label: "m22".to_owned(),
                aliases: Vec::new(),
                hint: None,
                admitted_ms: 1,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            }],
            deny_labels: vec!["m4".to_owned()],
            deny_nodes: Vec::new(),
        };
        let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
        let mut doc = adm.doc.clone();
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .expect("set roster");
        aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish roster");
        aterm_secrets::client::sync(&paths, None).expect("sync local copy");

        std::fs::write(&config, "[swarm]\nlabel = \"m4\"\n").unwrap();
        let facts_denied = SwarmSurveyFacts::collect_at(
            &state,
            Some(&home),
            Some(&config),
            0,
            false,
            None,
            1_793_160_000,
        );
        assert_eq!(facts_denied.label, None);

        // 2. When this recipient IS in the roster with a denied node_key, even a valid `[swarm] label = "m100"`
        //    in `aterm.toml` is rejected.
        let adm2 = aterm_secrets::client::admin_open(&paths).expect("admin open");
        let mut doc2 = adm2.doc.clone();
        let roster_denied_node = SwarmRoster {
            members: vec![SwarmMember {
                recipient: rec.clone(),
                vault_name: "m100".to_owned(),
                label: "m100".to_owned(),
                aliases: Vec::new(),
                hint: None,
                admitted_ms: 1,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some("ab".repeat(32)),
            }],
            deny_labels: Vec::new(),
            deny_nodes: vec!["ab".repeat(32)],
        };
        doc2.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            ROSTER_ENTRY,
            roster_denied_node.to_toml().as_bytes(),
        )
        .expect("set roster");
        aterm_secrets::client::admin_publish(&paths, &adm2, &doc2).expect("publish roster");
        aterm_secrets::client::sync(&paths, None).expect("sync local copy");
        std::fs::write(&config, "[swarm]\nlabel = \"m100\"\n").unwrap();
        let facts_denied_node = SwarmSurveyFacts::collect_at(
            &state,
            Some(&home),
            Some(&config),
            0,
            false,
            None,
            1_793_160_000,
        );
        assert_eq!(facts_denied_node.label, None);

        // 3. Once the node_key is not denied, `m100` resolves cleanly even without `aterm.toml`.
        let _ = std::fs::remove_file(&config);
        let adm3 = aterm_secrets::client::admin_open(&paths).expect("admin open");
        let mut doc3 = adm3.doc.clone();
        let mut roster_ok = roster_denied_node;
        roster_ok.deny_nodes.clear();
        doc3.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            ROSTER_ENTRY,
            roster_ok.to_toml().as_bytes(),
        )
        .expect("set roster");
        aterm_secrets::client::admin_publish(&paths, &adm3, &doc3).expect("publish roster");
        aterm_secrets::client::sync(&paths, None).expect("sync local copy");
        let facts_ok =
            SwarmSurveyFacts::collect_at(&state, Some(&home), None, 0, false, None, 1_793_160_000);
        assert_eq!(facts_ok.label.as_deref(), Some("m100"));

        running.stop();
    }
}
