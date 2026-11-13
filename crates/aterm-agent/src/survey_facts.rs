// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm survey facts (C16): host label, version, window liveness (`window` / `window_last`),
//! session count, per-row account health/enrollment/usage/last-use across pooled CLIs, pure-Rust
//! SSH host-key `SHA256:…` fingerprint, and vault-server subscriber last-heard records.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use crate::ai::connector::Cli;
use crate::harness::accounts;
use crate::harness::cli::{Env, PoolMoves, SurveyPool};
use crate::harness::repo_work::home::{self as repo_home, HomeRepo, HomeRepos};

const WINDOW_LAST_FILE: &str = "window-last";
const SSH_PUBKEY_MAX: usize = 16 << 10;

/// Per-account health, enrollment, usage and last-use observation carried in survey facts (§6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyAccountFact {
    /// Pooled CLI identifier (`claude`, `codex`, `grok`, `kimi`).
    pub pool: String,
    /// Account label in the roster (`accounts.toml` / `accounts-<cli>.toml`): the key `--host any`
    /// and the fleet census match rows by.
    pub id: String,
    /// The account's address, the name a reader shows it by (`email=`; [`SurveyAccount::email`]).
    /// `None` where the row and its place name none, and from an aterm older than the field.
    pub email: Option<String>,
    /// Canonical [`accounts::Condition`] word (`usable`, `limited`, `low`, `not-enrolled`, …).
    pub condition: String,
    /// Whether the login directory is enrolled on this machine.
    pub enrolled: bool,
    /// Active 5-hour window utilization percent, when observed and still running.
    pub usage_5h_pct: Option<u32>,
    /// Active 7-day weekly utilization percent, when observed and still running.
    pub usage_7d_pct: Option<u32>,
    /// Unix timestamp (seconds) when the weekly window resets, when known. Where its read named no
    /// reset, the latest it can: a week from that read.
    pub week_reset: Option<i64>,
    /// Unix timestamp (seconds) until which a standing wall blocks this account, when walled.
    pub walled_until: Option<i64>,
    /// Unix timestamp (seconds) when a session here last ran on this account.
    pub last_used: Option<i64>,
    /// Unix timestamp (seconds) when its usage figures were last read, when known. A row whose
    /// reading is old, or no newer than its newest wall, says `unseen`, not `usable`
    /// ([`crate::harness::cli::survey_accounts`]).
    pub read_at: Option<i64>,
    /// Whether a worker with no starter of its kind would start on it here (`launch=1|0`,
    /// [`crate::harness::accounts_pool::unstarted_launch_row`]). `None` from an aterm older than
    /// the field, which says nothing of it (no `launch=`, or `launch=-`).
    pub launch: Option<bool>,
    /// Claude Code's row: its vault token's state here
    /// ([`crate::harness::accounts_tokens::TokenState::word`], `docs/DESIGN-claude-setup-token-2026-11-05.md`
    /// §5.10). `None` for another pool, on a Mac with no vault configured, and from an aterm older
    /// than the field (no `token=`, or `token=-`).
    pub token: Option<String>,
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
    /// Host-checked release process identities and bounded local GitHub quota summaries.
    pub operation_lines: Vec<String>,
    /// Fleet node id (`node=` on `instance`), when enrolled in a fleet.
    pub node: Option<String>,
    /// Canonical machine label (`m<N>` or configured `[ai]`/`[fleet] machine` / vault host).
    pub label: Option<String>,
    /// The roster was unread this pass; a missing label is unknown rather than absent.
    pub roster_unread: bool,
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
    /// The account each supervised session was last read running on, by pool and session id: its
    /// row's address, else its label. Not on the wire: this instance's own session rows name it.
    pub session_accounts: BTreeMap<(String, String), String>,
    /// What this Mac's ledgers say of rotations (`moves_7d=`, `last_move=`). `None` from an aterm
    /// whose facts predate them or with a pool it could not read, which a reader takes as unknown,
    /// never as none.
    pub moves: Option<MoveFacts>,
    /// Subscriber last-heard records when this machine serves vaults (`~/.config/aterm/secrets/served`).
    pub server_heard: Vec<ServerHeardFact>,
    /// The home this aterm runs under, absolute (`home=`): where a fleet spawn's `cwd=~/…`
    /// resolves. `None` from an aterm that says nothing of it, which a reader takes as unknown.
    pub home: Option<String>,
    /// Free space on the home's volume, in MiB (`home_free_mb=`), read with these facts.
    pub home_free_mb: Option<u64>,
    /// The checkouts aterm's setup clones into the home, as its last read found them
    /// ([`repo_home::read_home`]: `repos=`, `repos_at=`, `repos_cut=` and one `repo` line
    /// each); `None` before a read finished, which a reader takes as unknown, never as none.
    pub repos: Option<HomeRepos>,
}

/// What a Mac's ledgers keep of rotations, across its pools: whether rotation has ever happened
/// here is the one thing a roster and a condition cannot say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveFacts {
    /// Rotations onto another row a session then proved, in the last seven days: it worked.
    pub proven_7d: usize,
    /// Rotations onto another row, in the last seven days, proved or not: each was an attempt.
    pub tried_7d: usize,
    /// The newest proved rotation its ledgers keep, Unix seconds.
    pub last: Option<i64>,
}

impl MoveFacts {
    fn of(moves: &PoolMoves, now: i64) -> Self {
        const WEEK_S: i64 = 7 * 24 * 60 * 60;
        let in_week = |times: &[i64]| {
            times
                .iter()
                .filter(|t| now.saturating_sub(**t) < WEEK_S)
                .count()
        };
        Self {
            proven_7d: in_week(&moves.proven),
            tried_7d: in_week(&moves.tried),
            last: moves.proven.iter().copied().max(),
        }
    }
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
                operation_lines: Vec::new(),
                node,
                label: None,
                roster_unread: false,
                version: aterm_stamp::app_version().to_owned(),
                window,
                window_last: window.then(|| u64::try_from(now.max(0)).unwrap_or(0)),
                sessions,
                ssh_host_key: cached_system_ssh_host_key(),
                accounts: Vec::new(),
                accounts_unread: Cli::ALL.iter().map(|cli| cli.id().to_owned()).collect(),
                session_accounts: BTreeMap::new(),
                moves: None,
                server_heard: Vec::new(),
                home: None,
                home_free_mb: None,
                repos: None,
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
            xdg_config_home: None,
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
        let window_last = read_window_last(&env.state, window, now_u64);
        let resolved_label = resolve_machine_label_checked(env);
        let roster_unread = resolved_label.is_err();
        let label = resolved_label.ok().flatten();
        let (accounts, accounts_unread, session_accounts, moved) = collect_account_facts(env);
        let server_heard = collect_server_heard(env);
        let home = env.home.as_deref().filter(|home| home.is_absolute());
        Self {
            operation_lines: crate::release_progress::survey(&env.state)
                .into_iter()
                .chain(aterm_ctl::github_usage::survey(
                    &env.state,
                    u64::try_from(env.now).unwrap_or(0),
                ))
                .collect(),
            node,
            label,
            roster_unread,
            version: aterm_stamp::app_version().to_owned(),
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts,
            moves: moved.map(|moves| MoveFacts::of(&moves, env.now)),
            server_heard,
            home: home.and_then(Path::to_str).map(str::to_owned),
            home_free_mb: home.and_then(free_mb),
            repos: home.and_then(repo_home::latest),
        }
    }

    /// Space-prefixed `key=value` tokens appended onto `survey`'s `instance …` header line: with
    /// `accounts=` counting the `account` lines that follow, and `accounts_unread=` naming the
    /// pools none of whose rows they list because their records could not be read (`-` for none).
    /// Volume free space (`home_free_mb=`) is a resource fact. A reply at `content` also adds
    /// the home location and its checkouts' count and read (`home=`, `repos=`, `repos_at=`,
    /// `repos_cut=`): work locations remain content, as a session's folder is.
    #[must_use]
    pub fn header_tokens(&self, content: bool) -> String {
        let home = if content {
            let repos = self.repos.as_ref();
            format!(
                " home={} repos={} repos_at={} repos_cut={}",
                word_token(self.home.as_deref()),
                repos.map_or_else(|| "-".to_owned(), |read| read.repos.len().to_string()),
                opt_u64_token(repos.map(|read| read.read_ms)),
                opt_u64_token(repos.and_then(|read| u64::try_from(read.cut).ok())),
            )
        } else {
            String::new()
        };
        format!(
            " label={} window={} window_last={} ssh_host_key={} home_free_mb={}{home} accounts={} \
             accounts_unread={} moves_7d={} move_tries_7d={} last_move={} roster_unread={}",
            word_token(self.label.as_deref()),
            u8::from(self.window),
            opt_u64_token(self.window_last),
            word_token(self.ssh_host_key.as_deref()),
            opt_u64_token(self.home_free_mb),
            self.accounts.len(),
            word_token(Some(&self.accounts_unread.join(","))),
            opt_u64_token(self.moves.and_then(|m| u64::try_from(m.proven_7d).ok())),
            opt_u64_token(self.moves.and_then(|m| u64::try_from(m.tried_7d).ok())),
            opt_i64_token(self.moves.and_then(|m| m.last)),
            u8::from(self.roster_unread),
        )
    }

    /// Take the home's checkouts as its latest read found them ([`repo_home::latest`]), so facts a
    /// listener keeps a while still say a read that ended since they were collected.
    pub fn take_latest_repos(&mut self) {
        if let Some(home) = self.home.as_deref() {
            self.repos = repo_home::latest(Path::new(home));
        }
    }

    /// Additional `account …`, `repo …` and `server_heard …` lines emitted in a `survey` reply,
    /// each account named by its label (`id=`) and its address (`email=`, shown whole, as the
    /// account registry shows it to every member); the `repo` lines only at `content`
    /// ([`Self::header_tokens`]).
    #[must_use]
    pub fn extra_lines(&self, content: bool) -> Vec<String> {
        let repos = match &self.repos {
            Some(read) if content => &read.repos[..],
            _ => &[][..],
        };
        let mut lines =
            Vec::with_capacity(self.accounts.len() + repos.len() + self.server_heard.len());
        lines.extend(self.operation_lines.iter().cloned());
        for acc in &self.accounts {
            lines.push(format!(
                "account pool={} id={} email={} condition={} enrolled={} pct5={} pct7={} \
                 week_reset={} walled_until={} last_used={} read_at={} launch={} token={}",
                aterm_codec::pct::encode(&acc.pool),
                aterm_codec::pct::encode(&acc.id),
                acc.email
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
                aterm_codec::pct::encode(&acc.condition),
                u8::from(acc.enrolled),
                opt_u32_token(acc.usage_5h_pct),
                opt_u32_token(acc.usage_7d_pct),
                opt_i64_token(acc.week_reset),
                opt_i64_token(acc.walled_until),
                opt_i64_token(acc.last_used),
                opt_i64_token(acc.read_at),
                acc.launch
                    .map_or("-", |launch| if launch { "1" } else { "0" }),
                acc.token
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            ));
        }
        for repo in repos {
            lines.push(format!(
                "repo name={} path={} head={} branch={} main={} behind={} fetched_at={} dirty={}",
                aterm_codec::pct::encode(&repo.name),
                word_token(repo.path.to_str()),
                word_token(repo.head.as_deref()),
                word_token(repo.branch.as_deref()),
                word_token(repo.main_ref.as_deref()),
                opt_u32_token(repo.behind),
                opt_u64_token(repo.fetched_ms),
                repo.dirty
                    .map_or("-", |dirty| if dirty { "1" } else { "0" }),
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
    /// heartbeats when no window socket is needed and by round-trip tests), at `facts`: it
    /// carries nothing of the home ([`Self::header_tokens`]).
    #[must_use]
    pub fn to_survey_reply(
        &self,
        pid: u32,
        host: Option<&str>,
        at_ms: u64,
        instances: usize,
        (access, drive): (aterm_types::fleet_access::FleetAccess, bool),
    ) -> String {
        self.to_survey_reply_at(false, pid, host, at_ms, instances, (access, drive))
    }

    /// [`Self::to_survey_reply`] at `content` where it is true (`level=content`), with the home's
    /// facts and checkouts. `access` is this machine's fleet access (`access=<word>`, on every
    /// header) and `drive` whether a grant admits the reader (`drive=1`): what the reader may do
    /// here is both.
    #[must_use]
    pub fn to_survey_reply_at(
        &self,
        content: bool,
        pid: u32,
        host: Option<&str>,
        at_ms: u64,
        instances: usize,
        (access, drive): (aterm_types::fleet_access::FleetAccess, bool),
    ) -> String {
        let extra = self.extra_lines(content);
        let count = 1 + extra.len();
        let host = word_token(host);
        let version = aterm_codec::pct::encode(&self.version);
        let node = word_token(self.node.as_deref());
        let drive_tok = if drive { " drive=1" } else { "" };
        let mut out = format!(
            "OK {count}\ninstance pid={pid} host={host} version={version} sessions={} \
             level={} cut=0 at_ms={at_ms} node={node} instances={instances}{} \
             access={}{drive_tok}\n",
            self.sessions,
            if content { "content" } else { "facts" },
            self.header_tokens(content),
            access.word(),
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
        let mut repos = Vec::new();
        let mut server_heard = Vec::new();
        let mut operation_lines = Vec::new();

        for raw in reply.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with("OK ") || line.starts_with("session ") {
                continue;
            }
            if line.starts_with("instance ") {
                instance = Some(parse_fields(line));
            } else if line.starts_with("account ") {
                accounts.push(parse_account_line(&parse_fields(line))?);
            } else if line.starts_with("repo ") {
                repos.push(parse_repo_line(&parse_fields(line))?);
            } else if line.starts_with("server_heard ") {
                server_heard.push(parse_server_heard_line(&parse_fields(line))?);
            } else if line.starts_with("release_line ") || line.starts_with("github_rate ") {
                operation_lines.push(line.to_owned());
            }
        }

        let header =
            instance.ok_or_else(|| "missing instance header in survey reply".to_owned())?;
        if let Some(raw_accounts) = field_raw(&header, "accounts") {
            let expected = crate::parse_uint::<usize>(raw_accounts)
                .ok_or_else(|| "invalid accounts field in instance header".to_owned())?;
            if expected != accounts.len() {
                return Err(format!(
                    "survey reply account count mismatch: header says {expected}, body has {}",
                    accounts.len()
                ));
            }
        }
        let repos = parse_repos(&header, repos)?;
        let version = required_nonempty(&header, "version")?;
        let sessions = crate::parse_uint::<usize>(required_raw(&header, "sessions")?)
            .ok_or_else(|| "invalid sessions field in instance header".to_owned())?;
        let window = parse_bool01(required_raw(&header, "window")?, "window")?;
        let window_last = parse_opt_u64(required_raw(&header, "window_last")?, "window_last")?;
        let label = optional_word(required_raw(&header, "label")?);
        let ssh_host_key = optional_word(required_raw(&header, "ssh_host_key")?);
        let node = field_raw(&header, "node").and_then(optional_word);
        // Optional: an aterm whose C16 facts predate it names no unread pool.
        let accounts_unread = field_raw(&header, "accounts_unread")
            .and_then(optional_word)
            .map_or_else(Vec::new, |pools| {
                pools
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            });

        // Optional: an aterm whose C16 facts predate them says nothing of its moves.
        let moves = field_raw(&header, "moves_7d")
            .and_then(optional_word)
            .map(|week| {
                Ok::<_, String>(MoveFacts {
                    proven_7d: crate::parse_uint::<usize>(&week)
                        .ok_or_else(|| "invalid moves_7d field in instance header".to_owned())?,
                    tried_7d: field_raw(&header, "move_tries_7d")
                        .and_then(crate::parse_uint::<usize>)
                        .ok_or_else(|| {
                            "invalid move_tries_7d field in instance header".to_owned()
                        })?,
                    last: field_raw(&header, "last_move")
                        .map_or(Ok(None), |raw| parse_opt_i64(raw, "last_move"))?,
                })
            })
            .transpose()?;

        Ok(Self {
            operation_lines,
            node,
            label,
            version,
            roster_unread: field_raw(&header, "roster_unread")
                .map_or(Ok(false), |raw| parse_bool01(raw, "roster_unread"))?,
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts: BTreeMap::new(),
            moves,
            server_heard,
            home: field_raw(&header, "home").and_then(optional_word),
            home_free_mb: field_raw(&header, "home_free_mb")
                .map_or(Ok(None), |raw| parse_opt_u64(raw, "home_free_mb"))?,
            repos,
        })
    }
}

/// The home's checkouts an `instance` header (`header`) and its `repo` lines (`rows`) say: `None`
/// where it says nothing of them (an aterm whose facts predate them) or none was read yet
/// (`repos=-`, with no line).
fn parse_repos(
    header: &[(String, String)],
    rows: Vec<HomeRepo>,
) -> Result<Option<HomeRepos>, String> {
    let Some(count) = field_raw(header, "repos").and_then(optional_word) else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err("survey reply has repo lines and no repos count".to_owned())
        };
    };
    let expected = crate::parse_uint::<usize>(&count)
        .ok_or_else(|| "invalid repos field in instance header".to_owned())?;
    if expected != rows.len() {
        return Err(format!(
            "survey reply repo count mismatch: header says {expected}, body has {}",
            rows.len()
        ));
    }
    let read_ms = parse_opt_u64(required_raw(header, "repos_at")?, "repos_at")?
        .ok_or_else(|| "empty `repos_at` field".to_owned())?;
    let cut = parse_opt_u64(required_raw(header, "repos_cut")?, "repos_cut")?
        .and_then(|cut| usize::try_from(cut).ok())
        .ok_or_else(|| "invalid `repos_cut` field".to_owned())?;
    Ok(Some(HomeRepos {
        read_ms,
        repos: rows,
        cut,
    }))
}

fn parse_repo_line(fields: &[(String, String)]) -> Result<HomeRepo, String> {
    Ok(HomeRepo {
        name: required_nonempty(fields, "name")?,
        path: required_nonempty(fields, "path")?.into(),
        head: optional_word(required_raw(fields, "head")?),
        branch: optional_word(required_raw(fields, "branch")?),
        main_ref: optional_word(required_raw(fields, "main")?),
        behind: parse_opt_u32(required_raw(fields, "behind")?, "behind")?,
        fetched_ms: parse_opt_u64(required_raw(fields, "fetched_at")?, "fetched_at")?,
        dirty: match required_raw(fields, "dirty")? {
            "-" => None,
            raw => Some(parse_bool01(raw, "dirty")?),
        },
    })
}

/// The free space on `home`'s volume, in MiB.
fn free_mb(home: &Path) -> Option<u64> {
    aterm_sysprobe::volumes::read_path(home, &aterm_sysprobe::volumes::table())
        .ok()
        .map(|reading| reading.free >> 20)
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
    crate::parse_uint::<u64>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

fn parse_opt_u32(raw: &str, key: &str) -> Result<Option<u32>, String> {
    if raw == "-" {
        return Ok(None);
    }
    crate::parse_uint::<u32>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

fn parse_opt_i64(raw: &str, key: &str) -> Result<Option<i64>, String> {
    if raw == "-" {
        return Ok(None);
    }
    crate::parse_int::<i64>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

fn parse_account_line(fields: &[(String, String)]) -> Result<SurveyAccountFact, String> {
    Ok(SurveyAccountFact {
        pool: required_nonempty(fields, "pool")?,
        id: required_nonempty(fields, "id")?,
        // A member older than the field sends none.
        email: field_raw(fields, "email")
            .filter(|raw| !raw.is_empty() && *raw != "-")
            .map(str::to_owned),
        condition: required_nonempty(fields, "condition")?,
        enrolled: parse_bool01(required_raw(fields, "enrolled")?, "enrolled")?,
        usage_5h_pct: parse_opt_u32(required_raw(fields, "pct5")?, "pct5")?,
        usage_7d_pct: parse_opt_u32(required_raw(fields, "pct7")?, "pct7")?,
        week_reset: parse_opt_i64(required_raw(fields, "week_reset")?, "week_reset")?,
        walled_until: parse_opt_i64(required_raw(fields, "walled_until")?, "walled_until")?,
        last_used: parse_opt_i64(required_raw(fields, "last_used")?, "last_used")?,
        // A member older than the field sends none.
        read_at: required_raw(fields, "read_at")
            .ok()
            .map_or(Ok(None), |raw| parse_opt_i64(raw, "read_at"))?,
        // A member older than the field sends none.
        launch: field_raw(fields, "launch")
            .filter(|raw| *raw != "-")
            .map(|raw| parse_bool01(raw, "launch"))
            .transpose()?,
        // A member older than the field sends none.
        token: field_raw(fields, "token")
            .filter(|raw| !raw.is_empty() && *raw != "-")
            .map(str::to_owned),
    })
}

fn parse_server_heard_line(fields: &[(String, String)]) -> Result<ServerHeardFact, String> {
    let vault = required_nonempty(fields, "vault")?;
    let subject = required_nonempty(fields, "subject")?;
    let at = crate::parse_uint::<u64>(required_raw(fields, "at")?)
        .ok_or_else(|| "invalid `at` field on server_heard".to_owned())?;
    Ok(ServerHeardFact { vault, subject, at })
}

fn read_window_last(state_dir: &Path, window: bool, now: u64) -> Option<u64> {
    if window {
        return Some(now);
    }
    let path = state_dir.join(WINDOW_LAST_FILE);
    crate::harness::accounts::read_bounded(&path, 256)
        .ok()
        .flatten()
        .and_then(|s| crate::parse_uint::<u64>(s.trim()))
        .filter(|at| *at <= now)
}

/// Record advisory window liveness from the window's background heartbeat, never a survey
/// request. Losing this hint on power loss is acceptable; it grants no authority and a live
/// window reports its current clock directly.
/// # Errors
/// The state directory could not be created, or the atomic publication failed.
pub fn record_window_last(state_dir: &Path, now: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    aterm_uds::atomic_file::write_flushed(
        &state_dir.join(WINDOW_LAST_FILE),
        format!("{now}\n").as_bytes(),
        0o600,
        None,
    )
}

#[cfg(unix)]
fn local_roster_denial(home: Option<&Path>) -> Result<(Vec<String>, bool, Option<String>), String> {
    let Some(home) = home else {
        return Ok((Vec::new(), false, None));
    };
    let paths = aterm_secrets::client::Paths::for_home(home);
    let roster = aterm_secrets::client::swarm_roster_until(
        &paths,
        std::time::Instant::now() + std::time::Duration::from_millis(250),
    )
    .map_err(|why| why.to_string())?;
    let recipient_denied = aterm_secrets::client::local_recipient_denied_in_roster(&paths);
    let label = if let Some(roster) = &roster {
        aterm_secrets::client::read_machine_identity(&paths)
            .map_err(|why| why.to_string())?
            .and_then(|identity| {
                roster
                    .member_by_recipient(&identity.recipient().to_bech32())
                    .filter(|member| !roster.is_member_denied(member))
                    .map(|member| member.label.clone())
            })
    } else {
        aterm_secrets::client::status(&paths)
            .ok()
            .and_then(|status| status.config.map(|config| config.host))
            .filter(|host| !host.trim().is_empty())
    };
    Ok((
        roster.map_or_else(Vec::new, |r| r.deny_labels),
        recipient_denied,
        label,
    ))
}

#[cfg(not(unix))]
fn local_roster_denial(
    _home: Option<&Path>,
) -> Result<(Vec<String>, bool, Option<String>), String> {
    Ok((Vec::new(), false, None))
}

/// This machine's canonical label as its survey facts name it, where it has one it may publish.
#[must_use]
pub fn machine_label() -> Option<String> {
    resolve_machine_label(&Env::from_process().ok()?)
}

fn resolve_machine_label(env: &Env) -> Option<String> {
    resolve_machine_label_checked(env).ok().flatten()
}

fn resolve_machine_label_checked(env: &Env) -> Result<Option<String>, String> {
    let (deny_labels, recipient_denied, vault_label) = local_roster_denial(env.home.as_deref())?;
    if recipient_denied {
        return Ok(None);
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
    if let Some(label) = env
        .config
        .as_deref()
        .and_then(|config| config_label(config, &is_denied_label))
    {
        return Ok(Some(label));
    }
    Ok(vault_label.filter(|h| !is_denied_label(h)))
}

/// The first of `[swarm] label`, `[ai] machine`, `[fleet] machine` and `[harness] machine` the
/// `aterm.toml` at `config` sets to a name `denied` does not deny, read by the rule every reader
/// of the file holds ([`aterm_types::dirs::read_aterm_config_text`]): a file group or others can
/// write names none.
fn config_label(config: &Path, denied: &dyn Fn(&str) -> bool) -> Option<String> {
    let text = aterm_types::dirs::read_aterm_config_text(config).ok()??;
    let table = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
    [
        ("swarm", "label"),
        ("ai", "machine"),
        ("fleet", "machine"),
        ("harness", "machine"),
    ]
    .into_iter()
    .find_map(|(section, key)| {
        table
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get(key))
            .and_then(aterm_toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && !denied(s))
            .map(str::to_owned)
    })
}

/// Each pooled CLI's rows, the pools that could not be read, the row each session runs on, as
/// `aterm harness accounts --json` reads them beside `env`'s `aterm.toml`, and when its ledgers
/// say it rotated. Claude Code's rows carry their vault token's state ([`claude_token_states`]).
fn collect_account_facts(env: &Env) -> AccountFacts {
    let mut facts = account_facts_of(Cli::ALL.into_iter().map(|cli| {
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
    }));
    let states = claude_token_states(env);
    for row in facts
        .0
        .iter_mut()
        .filter(|row| row.pool == Cli::Claude.id())
    {
        row.token = states.get(&row.id).cloned();
    }
    facts
}

/// Each Claude Code row's vault token state word by label (§5.10): one read of this Mac's vault
/// and its token table ([`crate::harness::accounts_token_view::states_by_label`]); none off Unix,
/// where no home is known, or where no vault is configured
/// ([`crate::harness::accounts_tokens::configured_vault`]), so such a Mac's rows say `token=-`
/// as every other surface says nothing of tokens there.
fn claude_token_states(env: &Env) -> BTreeMap<String, String> {
    #[cfg(unix)]
    {
        use crate::harness::accounts_token_view as view;
        let Some(home) = env.home.as_deref() else {
            return BTreeMap::new();
        };
        let store = accounts::Store::at(&env.state);
        let roster = env
            .config
            .as_deref()
            .and_then(|config| accounts::roster_beside_for(config, Cli::Claude));
        let Some(vault) = crate::harness::accounts_tokens::configured_vault(
            aterm_secrets::client::Paths::for_home(home),
        ) else {
            return BTreeMap::new();
        };
        view::states_by_label(
            &view::Places {
                store: &store,
                roster: roster.as_deref(),
                logins: &[],
                now: env.now,
                offers: view::OFFERED_HERE,
            },
            &vault,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = env;
        BTreeMap::new()
    }
}

/// What [`collect_account_facts`] reads: the rows, the unread pools, each session's row, and its
/// rotations, `None` where a pool or its ledger could not be read.
type AccountFacts = (
    Vec<SurveyAccountFact>,
    Vec<String>,
    BTreeMap<(String, String), String>,
    Option<PoolMoves>,
);

/// The account facts each pool's read makes: its rows, or its id among the unread where it could
/// not be read (a pool left out with no word would read as one holding no row).
fn account_facts_of(
    reads: impl IntoIterator<Item = (Cli, Result<SurveyPool, String>)>,
) -> AccountFacts {
    let mut rows = Vec::new();
    let mut unread = Vec::new();
    let mut sessions = BTreeMap::new();
    let mut moved = Some(PoolMoves::default());
    for (cli, read) in reads {
        let pool = cli.id().to_owned();
        let Ok(read) = read else {
            unread.push(pool);
            moved = None;
            continue;
        };
        moved = moved.zip(read.moves).map(|(mut all, more)| {
            all.tried.extend(more.tried);
            all.proven.extend(more.proven);
            all
        });
        let names: BTreeMap<String, String> = read
            .rows
            .iter()
            .filter_map(|row| Some((row.label.clone(), row.email.clone()?)))
            .collect();
        rows.extend(read.rows.into_iter().map(|row| SurveyAccountFact {
            pool: pool.clone(),
            email: row.email,
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
            read_at: row.read_at,
            launch: Some(row.launch),
            token: None,
        }));
        sessions.extend(read.sessions.into_iter().map(|(sid, label)| {
            let name = names.get(&label).cloned().unwrap_or(label);
            ((pool.clone(), sid), name)
        }));
    }
    (rows, unread, sessions, moved)
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

#[cfg(test)]
#[path = "survey_facts_tests.rs"]
mod tests;
