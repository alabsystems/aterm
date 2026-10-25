// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AI CLI account roster admission, usage/wall tracking, rotation advice, and state store: Claude
//! Code's pool, and one per pooled CLI ([`Cli`]) through the same parser, store and advice.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};
use aterm_phase::ResetKind;

pub use crate::ai::connector::Cli;
use crate::ai::connector::{Flow, Region, RowKeys, Seam};

use super::one_line;

/// The roster's file name, beside `aterm.toml`.
pub const ACCOUNTS_FILE: &str = "accounts.toml";

/// The variable a relaunch line carries to choose the login.
pub const LOGIN_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";

/// The most rows one roster carries, and the most accounts a state keeps.
pub const MAX_ACCOUNTS: usize = 32;

/// The longest label.
pub const LABEL_MAX: usize = 64;

/// The one word of the label grammar that is no label: the harness command's help word.
pub const RESERVED_LABEL: &str = "help";

/// The longest `dir` a row may name, in bytes.
pub const DIR_MAX: usize = 512;

/// The longest `email` a row may carry, in bytes (RFC 5321's path bound).
pub const EMAIL_MAX: usize = 254;

/// The longest `browser` profile name a row may carry, in characters.
pub const BROWSER_MAX: usize = 64;

/// The most bytes of `accounts.toml` that are read.
pub const MAX_ROSTER_BYTES: u64 = 256 * 1024;

/// The most bytes of `state.json` that are read.
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The state file's schema.
pub const SCHEMA: u64 = 1;

/// The most walls kept per account (the vendor names six limit types).
pub const MAX_WALLS: usize = 8;

/// The most sessions kept, newest first.
pub const MAX_SESSIONS: usize = 256;

/// The most rotation times kept (the bound itself is at most twice [`MAX_ACCOUNTS`] an hour).
pub const MAX_ROTATIONS: usize = 128;

/// The longest session id, machine id, window name and `held` word kept.
pub const WORD_MAX: usize = 64;

/// The longest reason a ledger row carries, in bytes.
pub const WHY_MAX: usize = 200;

/// The longest ledger line that is parsed.
pub const MAX_EVENT_LINE_BYTES: usize = 4096;

/// The ledger is cut back to [`LEDGER_KEEP_ROWS`] once it passes this size.
pub const LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// The rows a cut ledger keeps.
pub const LEDGER_KEEP_ROWS: usize = 1024;

/// A percent above this is no figure (the vendor paints whole percents).
pub const PCT_MAX: u32 = 1000;

/// A wall that names its reset stands this long past it.
pub const RESET_GRACE_S: i64 = 150;

/// A wall observed after its own reset stands this long from the observation.
pub const LATE_WALL_S: [i64; 2] = [10 * 60, 30 * 60];

/// A wall that names no reset stands this long from the observation.
pub const NO_RESET_WALL_S: [i64; 3] = [30 * 60, 2 * 3600, 6 * 3600];

/// A wall seen again more than this long after it stopped standing is a new wall.
pub const WALL_EPISODE_S: i64 = 6 * 3600;

/// The session window's share at which an account is `low`.
pub const LOW_SESSION_PCT: u32 = 90;

/// The week's share at which an account is `low`.
pub const LOW_WEEK_PCT: u32 = 98;

/// A session is not moved early ([`Sample::leave_early`]) off an account whose low window resets
/// within this long.
pub const RESET_SOON_S: i64 = 15 * 60;

/// A `low` account is moved into only when no account becomes usable within this long.
pub const LOW_ONLY_AFTER_S: i64 = 15 * 60;

/// A wait is for the earliest dedicated instant when that is within this long of the earliest
/// instant of any account.
pub const DEDICATED_WAIT_S: i64 = 15 * 60;

/// An account holding a reset offer that expires within this long is not left early.
pub const OFFER_EXPIRING_S: i64 = 24 * 3600;

/// A reset claim with no outcome is in flight this long ([`reset_in_flight`]) and no longer.
pub const RESET_CLAIM_S: i64 = 10 * 60;

/// How long before a process's start no other login may have run for its reset to be allowed
/// ([`reset_allowed`], [`Account::ran_at`]).
pub const RESET_PROFILE_S: i64 = 25 * 3600;

/// The probe is due no sooner than this after the last sample or claim.
pub const PROBE_MIN_S: i64 = 3 * 60;

/// No account is probed within this long of any account's claim.
pub const PROBE_APART_S: i64 = 90;

/// The probe is due no later than this after the last sample.
pub const PROBE_MAX_S: i64 = 20 * 60;

/// The probe's interval when no burn is known.
pub const PROBE_DEFAULT_S: i64 = 10 * 60;

/// The window the machine's rotation bound counts over.
pub const ROTATION_WINDOW_S: i64 = 3600;

/// How long the one session on an account that has not served a response yet holds every other
/// session back ([`StayWhy::Canary`]).
pub const CANARY_HOLD_S: i64 = 10 * 60;

/// The most rotations whose undo is kept ([`State::undo`]).
pub const MAX_UNDO: usize = 8;

/// How far ahead of the reader's clock an observation's `at` may stand.
pub const STAMP_SKEW_S: i64 = 5 * 60;

/// The session window's length (the vendor's `five_hour`, `windowSeconds: 18000`).
pub const SESSION_WINDOW_S: i64 = 5 * 3600;

/// The week's length.
pub const WEEK_WINDOW_S: i64 = 7 * 24 * 3600;

/// How far past its own row a wall's reset may stand and still be believed ([`note_wall`],
/// [`WEEK_WINDOW_S`]).
pub const RESET_BELIEVED_S: i64 = 8 * 24 * 3600;

/// A session's row older than this is dropped when the state is saved ([`drop_stale_sessions`]).
pub const SESSION_KEEP_S: i64 = 30 * 24 * 3600;

/// How often a session's loop says again what its screen says of usage while that shows unchanged.
pub const CREDITS_RETELL_S: i64 = 10 * 60;

/// How old an answer's row must be for the screen read now to have drawn its notices
/// ([`UsageSaid::at`]).
pub const NOTICES_DRAWN_S: i64 = 2;

/// Credits are taken as in use this long past the newest answer seen served on them
/// ([`CreditsUse::stands`], [`CREDITS_RETELL_S`]).
pub const CREDITS_STALE_S: i64 = 3 * CREDITS_RETELL_S;

/// The weekly window counts as nearly full from this figure ([`windows_full`]).
pub const WINDOW_FULL_PCT: u32 = 90;

/// The weekly window counts as nearly full only while its reset is more than this far off.
pub const WINDOW_FULL_AHEAD_S: i64 = 24 * 3600;

/// The pauses of a bounded wait for `accounts.lock` ([`Store::acquire_within`]).
pub const LOCK_LADDER: [std::time::Duration; 9] = {
    const fn ms(n: u64) -> std::time::Duration {
        std::time::Duration::from_millis(n)
    }
    [
        ms(10),
        ms(20),
        ms(40),
        ms(80),
        ms(150),
        ms(300),
        ms(400),
        ms(500),
        ms(500),
    ]
};

/// What a bounded take answers when the lock stayed held ([`Store::transact_within`],
/// [`Recorded::lock_held`]).
pub const LOCK_HELD: &str = "the accounts lock is held";

/// The window name a candidate that failed with no wall of its own is struck under
/// ([`Strike::Failed`]).
pub const STRUCK_WINDOW: &str = "struck";

/// The window name a wall is recorded under when its row names no limit type and its text names
/// none either ([`record_wall`]).
pub const UNNAMED_WINDOW: &str = "limit";

/// The limit type of the five-hour window ([`Account::warned_session`]).
pub const SESSION_LIMIT: &str = "five_hour";

/// The limit type of the week of all models ([`Account::warned_week`]).
pub const WEEK_LIMIT: &str = "seven_day";

/// The variable that moves a process's Claude home ([`LOGIN_ENV`]).
pub const HOME_ENV: &str = "CLAUDE_CONFIG_DIR";

/// The variable Claude Code's `os.homedir()` reads.
pub const HOMEDIR_ENV: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

/// The variables that give a process a credential or a provider of its own
/// ([`super::netwatch::ROUTE_KEYS`], [`ProcessLogin::of_env`]).
pub const CREDENTIAL_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "CLAUDE_CODE_HOST_AUTH_ENV_VAR",
];

/// The variables of the vendor's `profile` source.
pub const PROFILE_KEYS: &[&str] = &[
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_IDENTITY_TOKEN",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_CONFIG_DIR",
];

/// Contention class (`dedicated` or `shared`) of a Claude Code account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Class {
    /// This machine's alone.
    #[default]
    Dedicated,
    /// Drawn on from elsewhere too.
    Shared,
}

impl Class {
    /// Canonical `accounts.toml` string for this class.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::Dedicated => "dedicated",
            Class::Shared => "shared",
        }
    }

    /// Parses a [`Class`] from its `accounts.toml` string.
    #[must_use]
    pub fn parse(word: &str) -> Option<Class> {
        match word {
            "dedicated" => Some(Class::Dedicated),
            "shared" => Some(Class::Shared),
            _ => None,
        }
    }
}

/// Admitted `accounts.toml` row, optionally bound to a renewed login directory via [`bound`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// The only name the account has anywhere.
    pub label: String,
    /// Dedicated or shared.
    pub class: Class,
    /// The login's directory, as a process's [`LOGIN_ENV`] holds it; `None` is the default login.
    pub dir: Option<String>,
    /// The account's email ([`email_matches`]).
    pub email: Option<String>,
    /// The browser profile that holds the account's claude.ai session.
    pub browser: Option<String>,
    /// The logins a renewal here bound this row away from ([`Binding::was`], [`bound`]).
    pub was: Vec<Option<String>>,
    /// Its vendor keys ([`RowKeys`]): none on a Claude Code row.
    pub vendor: Vendor,
}

/// A roster row's vendor keys, each admitted only by the CLI that takes it ([`RowKeys`],
/// [`Roster::parse_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vendor {
    /// `region` (Kimi Code): the account system the row's email belongs to.
    pub region: Option<Region>,
    /// `flow` (Codex, Grok): the sign-in flow the row names; `None` takes the CLI's default.
    pub flow: Option<Flow>,
}

impl Row {
    /// The sign-in flow of this `cli` row: its own `flow`, else the CLI's default, `None` for a
    /// CLI that takes no `flow`.
    #[must_use]
    pub fn flow(&self, cli: Cli) -> Option<Flow> {
        match cli.accounts().keys {
            RowKeys::Flow(default) => Some(self.vendor.flow.unwrap_or(default)),
            RowKeys::Plain | RowKeys::Region => None,
        }
    }
}

/// Admitted `accounts.toml` roster in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    /// The rows, in file order.
    pub rows: Vec<Row>,
}

/// Returns whether `label` matches `[a-z0-9][a-z0-9._-]{0,63}` and is not [`RESERVED_LABEL`].
#[must_use]
pub fn admissible_label(label: &str) -> bool {
    label_refusal(label).is_none()
}

/// Returns the refusal reason when `label` is not a valid account label.
#[must_use]
pub fn label_refusal(label: &str) -> Option<&'static str> {
    let bytes = label.as_bytes();
    let word = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let in_grammar = bytes.len() <= LABEL_MAX
        && bytes.first().is_some_and(word)
        && bytes
            .iter()
            .all(|b| word(b) || matches!(b, b'.' | b'_' | b'-'));
    if !in_grammar {
        Some("is outside [a-z0-9][a-z0-9._-]{0,63}")
    } else if label == RESERVED_LABEL {
        Some("is the harness command's help word, which no account can be named by")
    } else {
        None
    }
}

fn dir_refusal(dir: &str) -> Option<String> {
    if dir
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a control character".to_string())
    } else if dir.len() > DIR_MAX {
        Some(format!("is longer than {DIR_MAX} bytes"))
    } else if !Path::new(dir).is_absolute() {
        Some(
            "is relative; the login is named by this exact string, which must not depend on \
             where a relaunch starts"
                .to_string(),
        )
    } else {
        None
    }
}

/// Returns the refusal reason when `email` is not a valid roster email address.
#[must_use]
pub fn email_refusal(email: &str) -> Option<&'static str> {
    if email.len() > EMAIL_MAX {
        Some("is longer than 254 bytes")
    } else if email
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a space or a control character")
    } else {
        match email.split_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
                (domain.contains('@')).then_some("holds more than one @")
            }
            _ => Some("is not one address (`name@domain`)"),
        }
    }
}

/// Returns the refusal reason when `browser` is not a valid browser profile name.
#[must_use]
pub fn browser_refusal(browser: &str) -> Option<&'static str> {
    if browser.trim().is_empty() {
        Some("is empty")
    } else if browser.chars().count() > BROWSER_MAX {
        Some("is longer than 64 characters")
    } else if browser
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a control character")
    } else {
        None
    }
}

/// Compares two email addresses for ASCII case-insensitive equality.
#[must_use]
pub fn email_matches(row: &str, answered: &str) -> bool {
    row.eq_ignore_ascii_case(answered.trim())
}

fn shown(text: &str) -> String {
    one_line(text, 80)
}

impl Roster {
    /// Parses and validates an `accounts.toml` document.
    /// # Errors
    ///
    /// Each names the row and the reason: the text is not TOML; an unknown top-level key or
    /// table; `account` is not `[[account]]` rows; more than [`MAX_ACCOUNTS`] rows; a row with an
    /// unknown key, with no label, an inadmissible one ([`RESERVED_LABEL`] by name) or a repeated
    /// one, with no class or one that is neither `dedicated` nor `shared`, with a `dir` that is
    /// not a string, is relative, holds a control character, is longer than [`DIR_MAX`] bytes or
    /// repeats another row's; with an `email` that is not one address ([`email_refusal`]; never
    /// quoted) or repeats another row's, or a `browser` that is not a profile's name
    /// ([`browser_refusal`]); or more than one row with no `dir`.
    pub fn parse(text: &str) -> Result<Roster, String> {
        Roster::parse_for(Cli::Claude, text)
    }

    /// Parses and validates `cli`'s roster document as [`Roster::parse`] does, each refusal
    /// naming that CLI's file, with the vendor keys it takes ([`RowKeys`]) admitted.
    /// # Errors
    ///
    /// As [`Roster::parse`], and: a vendor key this CLI does not take (Claude Code takes none);
    /// a `region` other than `global` or `mainland-cn`, or a Kimi row with an `email` and no
    /// `region`; a `flow` other than `browser` or `device`.
    pub fn parse_for(cli: Cli, text: &str) -> Result<Roster, String> {
        let file = cli.accounts().roster_file;
        let doc = aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|e| {
            format!(
                "{file}: not valid TOML ({})",
                shown(e.to_string().lines().next().unwrap_or(""))
            )
        })?;
        let Some(top) = doc.as_table() else {
            return Err(format!("{file}: not a TOML document"));
        };
        for (key, value) in top {
            if key != "account" {
                let what = if value.as_table().is_some() {
                    "table"
                } else {
                    "key"
                };
                return Err(format!(
                    "{file}: unknown {what} `{}`; this file holds [[account]] rows and \
                     nothing else",
                    shown(key)
                ));
            }
        }
        let items = match top.get("account") {
            None => return Ok(Roster::default()),
            Some(aterm_toml::Value::Array(items)) => items,
            Some(other) => {
                return Err(format!(
                    "{file}: `account` is a TOML {}, not [[account]] rows",
                    other.type_str()
                ));
            }
        };
        if items.len() > MAX_ACCOUNTS {
            return Err(format!(
                "{file}: {} rows; a roster holds at most {MAX_ACCOUNTS}",
                items.len()
            ));
        }
        let mut rows: Vec<Row> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let row = row_of(cli, item, i + 1)?;
            if let Some(twin) = rows.iter().position(|r| r.label == row.label) {
                return Err(format!(
                    "{file}: label `{}` is listed twice (rows {} and {})",
                    row.label,
                    twin + 1,
                    i + 1
                ));
            }
            if let Some(twin) = rows.iter().find(|r| {
                r.email
                    .as_deref()
                    .zip(row.email.as_deref())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }) {
                return Err(format!(
                    "{file}: rows `{}` and `{}` name one email; an account is one row",
                    twin.label, row.label
                ));
            }
            if let Some(twin) = rows.iter().find(|r| r.dir == row.dir) {
                return Err(match &row.dir {
                    Some(_) => format!(
                        "{file}: rows `{}` and `{}` name one dir; a login is one account",
                        twin.label, row.label
                    ),
                    None => format!(
                        "{file}: rows `{}` and `{}` both name no dir; only one row is \
                         this machine's own login",
                        twin.label, row.label
                    ),
                });
            }
            rows.push(row);
        }
        Ok(Roster { rows })
    }

    /// Loads the roster at `path`; an absent file is an empty roster.
    /// # Errors
    ///
    /// The file exists and cannot be read, is larger than [`MAX_ROSTER_BYTES`], is not UTF-8,
    /// or does not admit ([`Roster::parse`]).
    pub fn load(path: &Path) -> Result<Roster, String> {
        Roster::load_for(Cli::Claude, path)
    }

    /// Loads `cli`'s roster at `path` ([`Roster::load`], [`Roster::parse_for`]).
    /// # Errors
    ///
    /// As [`Roster::load`], the text parsed by [`Roster::parse_for`].
    pub fn load_for(cli: Cli, path: &Path) -> Result<Roster, String> {
        match roster_text(path)? {
            None => Ok(Roster::default()),
            Some(text) => Roster::parse_for(cli, &text),
        }
    }

    /// Looks up the roster row with `label`.
    #[must_use]
    pub fn row(&self, label: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.label == label)
    }

    /// Looks up the roster row matching a process's [`LOGIN_ENV`] value.
    #[must_use]
    pub fn row_for_login(&self, login: Option<&str>) -> Option<&Row> {
        match login {
            None | Some("") => self.rows.iter().find(|r| r.dir.is_none()),
            Some(dir) => self.rows.iter().find(|r| r.dir.as_deref() == Some(dir)),
        }
    }

    /// Returns whether the roster lists two or more accounts.
    #[must_use]
    pub const fn several(&self) -> bool {
        self.rows.len() >= 2
    }

    /// Returns the login directory of a single-row roster when it is non-default.
    #[must_use]
    pub fn lone_login(&self) -> Option<&str> {
        match self.rows.as_slice() {
            [row] => row.dir.as_deref().filter(|dir| !dir.is_empty()),
            _ => None,
        }
    }

    /// Synthetic single-row roster representing this machine's default login.
    #[must_use]
    pub fn own() -> Roster {
        Roster {
            rows: vec![Row {
                label: super::cli::ACCOUNTS_OWN_LABEL.to_owned(),
                ..Row::default()
            }],
        }
    }

    /// Finds the roster row whose login was moved away from `login` by a renewal on this machine.
    #[must_use]
    pub fn row_left(&self, login: Option<&str>) -> Option<&Row> {
        let login = login.filter(|l| !l.is_empty());
        if self.row_for_login(login).is_some() {
            return None;
        }
        self.rows
            .iter()
            .find(|row| row.was.iter().any(|was| was.as_deref() == login))
    }
}

/// Reads the raw text of the roster file at `path` ([`MAX_ROSTER_BYTES`]).
/// # Errors
///
/// The file exists and cannot be read, is larger than [`MAX_ROSTER_BYTES`]
/// or is not UTF-8 — named with its path.
pub fn roster_text(path: &Path) -> Result<Option<String>, String> {
    read_bounded(path, MAX_ROSTER_BYTES).map_err(|e| format!("{}: {e}", path.display()))
}

fn row_of(cli: Cli, item: &aterm_toml::Value, n: usize) -> Result<Row, String> {
    let file = cli.accounts().roster_file;
    let Some(table) = item.as_table() else {
        return Err(format!(
            "{file}: row {n} is a TOML {}, not an [[account]] table",
            item.type_str()
        ));
    };
    let label = match table.get("label") {
        Some(aterm_toml::Value::String(label)) => label,
        Some(other) => {
            return Err(format!(
                "{file}: row {n}: its label is a TOML {}, not a string",
                other.type_str()
            ));
        }
        None => return Err(format!("{file}: row {n} has no label")),
    };
    if let Some(why) = label_refusal(label) {
        return Err(format!("{file}: row {n}: label `{}` {why}", shown(label)));
    }
    let keys = cli.accounts().keys;
    let vendor_key = match keys {
        RowKeys::Plain => None,
        RowKeys::Region => Some("region"),
        RowKeys::Flow(_) => Some("flow"),
    };
    if let Some(key) = table.keys().find(|k| {
        !matches!(k.as_str(), "label" | "class" | "dir" | "email" | "browser")
            && Some(k.as_str()) != vendor_key
    }) {
        let holds = vendor_key.map_or_else(
            || "email and browser".to_owned(),
            |key| format!("email, browser and {key}"),
        );
        return Err(format!(
            "{file}: row `{label}` has an unknown key `{}`; a row holds label, class, \
             dir, {holds}",
            shown(key)
        ));
    }
    let class = match table.get("class") {
        Some(aterm_toml::Value::String(word)) => Class::parse(word).ok_or_else(|| {
            format!(
                "{file}: row `{label}`: class `{}` is neither `dedicated` nor `shared`",
                shown(word)
            )
        })?,
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its class is a TOML {}, not `dedicated` or \
                 `shared`",
                other.type_str()
            ));
        }
        None => {
            return Err(format!(
                "{file}: row `{label}` has no class (`dedicated` or `shared`)"
            ));
        }
    };
    let dir = match table.get("dir") {
        None => None,
        Some(aterm_toml::Value::String(dir)) => {
            if let Some(why) = dir_refusal(dir) {
                return Err(format!("{file}: row `{label}`: its dir {why}"));
            }
            Some(dir.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its dir is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let email = match table.get("email") {
        None => None,
        Some(aterm_toml::Value::String(email)) => {
            if let Some(why) = email_refusal(email) {
                return Err(format!("{file}: row `{label}`: its email {why}"));
            }
            Some(email.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its email is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let browser = match table.get("browser") {
        None => None,
        Some(aterm_toml::Value::String(browser)) => {
            if let Some(why) = browser_refusal(browser) {
                return Err(format!("{file}: row `{label}`: its browser {why}"));
            }
            Some(browser.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its browser is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let word = |key: &str| match table.get(key) {
        None => Ok(None),
        Some(aterm_toml::Value::String(word)) => Ok(Some(word.as_str())),
        Some(other) => Err(format!(
            "{file}: row `{label}`: its {key} is a TOML {}, not a string",
            other.type_str()
        )),
    };
    let vendor = match keys {
        RowKeys::Plain => Vendor::default(),
        RowKeys::Region => {
            let region = word("region")?
                .map(|w| {
                    Region::parse(w).ok_or_else(|| {
                        format!(
                            "{file}: row `{label}`: region `{}` is neither `global` (kimi.ai) \
                             nor `mainland-cn` (kimi.com)",
                            shown(w)
                        )
                    })
                })
                .transpose()?;
            if region.is_none() && email.is_some() {
                return Err(format!(
                    "{file}: row `{label}` has an email and no region: name `global` (kimi.ai) \
                     or `mainland-cn` (kimi.com), since a sign-in on the other region's site can \
                     make a new account"
                ));
            }
            Vendor { region, flow: None }
        }
        RowKeys::Flow(_) => Vendor {
            region: None,
            flow: word("flow")?
                .map(|w| {
                    Flow::parse(w).ok_or_else(|| {
                        format!(
                            "{file}: row `{label}`: flow `{}` is neither `browser` nor `device`",
                            shown(w)
                        )
                    })
                })
                .transpose()?,
        },
    };
    Ok(Row {
        label: label.clone(),
        class,
        dir,
        email,
        browser,
        was: Vec::new(),
        vendor,
    })
}

/// Returns the `(`[`LOGIN_ENV`]`, value)` pair exported on a relaunch line for `row`.
#[must_use]
pub fn login_env(row: &Row) -> (&'static str, String) {
    (LOGIN_ENV, row.dir.clone().unwrap_or_default())
}

/// What a launch of `cli` under `row` sets ([`Seam`]): the seam's variable, and its value, or
/// `None` for the machine's default login (no `dir`), whose launch unsets the variable. A
/// [`Seam::LoginFile`] value names the file in the row's directory, the others the directory.
#[must_use]
pub fn login_env_for(cli: Cli, row: &Row) -> (&'static str, Option<String>) {
    let seam = cli.accounts().seam;
    let value = row
        .dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(|dir| match seam {
            Seam::LoginFile { file, .. } => {
                Path::new(dir).join(file).to_string_lossy().into_owned()
            }
            Seam::LoginDir { .. } | Seam::Overlay { .. } => dir.to_owned(),
        });
    (seam.var(), value)
}

fn row_refusal(row: &Row) -> Option<String> {
    let label = &row.label;
    if let Some(why) = label_refusal(label) {
        return Some(format!("label `{}` {why}", shown(label)));
    }
    if let Some(why) = row.dir.as_deref().and_then(dir_refusal) {
        return Some(format!("row `{label}`: its dir {why}"));
    }
    if let Some(why) = row.email.as_deref().and_then(email_refusal) {
        return Some(format!("row `{label}`: its email {why}"));
    }
    row.browser
        .as_deref()
        .and_then(browser_refusal)
        .map(|why| format!("row `{label}`: its browser {why}"))
}

/// Formats a single `[[account]]` TOML table, or `None` if `label` or `dir` is invalid.
#[must_use]
pub fn row_toml(label: &str, class: Class, dir: Option<&str>) -> Option<String> {
    row_text(&Row {
        label: label.to_owned(),
        class,
        dir: dir.map(str::to_owned),
        ..Row::default()
    })
}

/// Formats a complete [`Row`] (including `email` and `browser`) as an `[[account]]` TOML block.
#[must_use]
pub fn row_text(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    let mut out = format!(
        "[[account]]\nlabel = \"{}\"\nclass = \"{}\"\n",
        row.label,
        row.class.as_str()
    );
    let mut put = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            out.push_str(&format!("{key} = {}\n", aterm_toml::Value::from(value)));
        }
    };
    put("dir", row.dir.as_deref());
    put("email", row.email.as_deref());
    put("browser", row.browser.as_deref());
    put("region", row.vendor.region.map(Region::as_str));
    put("flow", row.vendor.flow.map(Flow::as_str));
    Some(out)
}

/// Formats a single account row as an inline TOML table.
#[must_use]
pub fn row_inline(label: &str, class: Class, dir: Option<&str>) -> Option<String> {
    row_inline_of(&Row {
        label: label.to_owned(),
        class,
        dir: dir.map(str::to_owned),
        ..Row::default()
    })
}

/// What a row's `email` is written as in a row to add by hand ([`row_inline_of`]).
pub const EMAIL_BY_HAND: &str = "<the --email you gave>";

fn row_inline_of(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    let mut out = format!(
        "{{ label = \"{}\", class = \"{}\"",
        row.label,
        row.class.as_str()
    );
    if let Some(dir) = &row.dir {
        out.push_str(&format!(
            ", dir = {}",
            aterm_toml::Value::from(dir.as_str())
        ));
    }
    if row.email.is_some() {
        out.push_str(&format!(
            ", email = {}",
            aterm_toml::Value::from(EMAIL_BY_HAND)
        ));
    }
    if let Some(browser) = &row.browser {
        out.push_str(&format!(
            ", browser = {}",
            aterm_toml::Value::from(browser.as_str())
        ));
    }
    for (key, word) in [
        ("region", row.vendor.region.map(Region::as_str)),
        ("flow", row.vendor.flow.map(Flow::as_str)),
    ] {
        if let Some(word) = word {
            out.push_str(&format!(", {key} = {}", aterm_toml::Value::from(word)));
        }
    }
    out.push_str(" }");
    Some(out)
}

/// Appends a new `(label, class, dir)` row to `existing` `accounts.toml` text and validates the
/// result ([`append_rows`]).
/// # Errors
///
/// As [`append_rows`], of which this is the one-row case.
pub fn append_row(
    existing: &str,
    label: &str,
    class: Class,
    dir: Option<&str>,
) -> Result<String, String> {
    append_rows(
        existing,
        &[Row {
            label: label.to_owned(),
            class,
            dir: dir.map(str::to_owned),
            ..Row::default()
        }],
    )
}

/// Appends `rows` in order to `existing` `accounts.toml` text and validates the result.
/// # Errors
///
/// A row would not admit, `existing` does not admit, or the whole does not (a repeated label,
/// `dir` or `email`, a second row with no `dir`, more than [`MAX_ACCOUNTS`] rows), each as
/// [`Roster::parse`] names it, so nothing is written that the next read refuses. A roster that
/// defines `account` inline (`account = [ { … } ]`, or `account = []`) refuses too, since an
/// `[[account]]` table cannot extend that array: the refusal carries every row to add by hand
/// ([`row_inline`]), in order, an email written as [`EMAIL_BY_HAND`].
pub fn append_rows(existing: &str, rows: &[Row]) -> Result<String, String> {
    append_rows_for(Cli::Claude, existing, rows)
}

/// Appends `rows` in order to `cli`'s roster text and validates the result ([`append_rows`],
/// [`Roster::parse_for`]).
/// # Errors
///
/// As [`append_rows`], each refusal naming `cli`'s roster file and parsed by
/// [`Roster::parse_for`].
pub fn append_rows_for(cli: Cli, existing: &str, rows: &[Row]) -> Result<String, String> {
    let file = cli.accounts().roster_file;
    if let Some(why) = rows.iter().find_map(row_refusal) {
        return Err(why);
    }
    Roster::parse_for(cli, existing)?;
    let mut out = existing.to_string();
    for row in rows {
        let text = row_text(row).ok_or_else(|| format!("row `{}` does not admit", row.label))?;
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&text);
    }
    if aterm_toml::from_str::<aterm_toml::Value>(&out).is_err() {
        let by_hand: Vec<String> = rows.iter().filter_map(row_inline_of).collect();
        let these = if by_hand.len() == 1 {
            "this row"
        } else {
            "these rows"
        };
        return Err(format!(
            "{file} defines `account` inline (`account = [ … ]`), which an [[account]] \
             table cannot extend; rewrite its rows as [[account]] tables, or add {these} to \
             that array by hand: {}",
            by_hand.join(", ")
        ));
    }
    Roster::parse_for(cli, &out)?;
    Ok(out)
}

/// Returns the set of roster labels whose login directories are enrolled on this machine.
#[must_use]
pub fn enrolled(roster: &Roster) -> BTreeSet<String> {
    roster
        .rows
        .iter()
        .filter(|r| r.dir.as_deref().is_none_or(|d| login_made(Path::new(d))))
        .map(|r| r.label.clone())
        .collect()
}

/// Returns the set of `cli`'s roster labels whose logins are made on this machine ([`enrolled`]
/// for Claude Code's): another CLI's row is enrolled once its directory holds the file the CLI
/// keeps its login in ([`crate::ai::connector::Connector::stores`]), since an overlay home holds
/// links and scratch before any sign-in.
#[must_use]
pub fn enrolled_for(cli: Cli, roster: &Roster) -> BTreeSet<String> {
    if cli == Cli::Claude {
        return enrolled(roster);
    }
    let stores = cli.connector().stores;
    roster
        .rows
        .iter()
        .filter(|r| {
            r.dir
                .as_deref()
                .is_none_or(|d| stores.iter().any(|f| Path::new(d).join(f).is_file()))
        })
        .map(|r| r.label.clone())
        .collect()
}

/// Returns whether `dir` is a non-empty directory containing an initialized login.
#[must_use]
pub fn login_made(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .is_ok_and(|mut entries| entries.next().is_some_and(|entry| entry.is_ok()))
}

fn is_enrolled(row: &Row, enrolled: &BTreeSet<String>) -> bool {
    row.dir.is_none() || enrolled.contains(&row.label)
}

/// Returns whether the roster at `path` admits and lists at least two accounts.
#[must_use]
pub fn several_listed(path: Option<&Path>) -> bool {
    path.and_then(|path| Roster::load(path).ok())
        .is_some_and(|roster| roster.several())
}

/// Loads the bound [`Roster`] used to attribute usage-tap readings at `now`.
#[must_use]
pub fn tap_roster(path: Option<&Path>, store: &Store, now: i64) -> Option<Roster> {
    let roster = match path.map(Roster::load) {
        None => return Some(Roster::own()),
        Some(read) => read.ok()?,
    };
    if roster.rows.is_empty() {
        return Some(Roster::own());
    }
    store.bound(&roster, now).ok()
}

/// Returns [`Roster::lone_login`] for the bound roster at `path`.
#[must_use]
pub fn lone_login_at(path: Option<&Path>, store: &Store, now: i64) -> Option<String> {
    let roster = Roster::load(path?).ok().filter(|r| r.rows.len() == 1)?;
    store
        .bound(&roster, now)
        .ok()?
        .lone_login()
        .map(str::to_owned)
}

/// Every login the roster at `path` names as `store` binds it at `now`: each row's `dir` and each
/// login a renewal bound a row away from ([`Row::was`]). These are the usage tap's logins where
/// the machine has no lone one ([`lone_login_at`]). Empty when the roster lists fewer than two
/// rows (one row follows only its current login) or the store's logins cannot be read, since
/// [`tap_roster`] and the footer's account gate refuse that roster too.
#[must_use]
pub fn listed_logins_at(path: Option<&Path>, store: &Store, now: i64) -> Vec<String> {
    let Some(roster) = path
        .and_then(|path| Roster::load(path).ok())
        .filter(Roster::several)
    else {
        return Vec::new();
    };
    let Ok(roster) = store.bound(&roster, now) else {
        return Vec::new();
    };
    roster
        .rows
        .iter()
        .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
        .filter(|dir| !dir.is_empty())
        .cloned()
        .collect()
}

/// Returns the `accounts.toml` path sibling to the given `aterm.toml` `config` path.
#[must_use]
pub fn roster_beside(config: &Path) -> Option<PathBuf> {
    Some(config.parent()?.join(ACCOUNTS_FILE))
}

/// Returns `cli`'s roster path ([`Cli`]) sibling to the given `aterm.toml` `config` path.
#[must_use]
pub fn roster_beside_for(config: &Path, cli: Cli) -> Option<PathBuf> {
    Some(config.parent()?.join(cli.accounts().roster_file))
}

/// Returns the default `accounts.toml` path beside the active `aterm.toml`.
#[must_use]
pub fn default_roster_path() -> Option<PathBuf> {
    roster_beside(&aterm_types::dirs::aterm_config_path()?)
}

/// Login directory and credential-override facts extracted from a process's environment and
/// settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLogin {
    /// The value of [`LOGIN_ENV`] where it is set and not empty.
    pub login: Option<String>,
    /// Its Claude home is the default one ([`HOME_ENV`]).
    pub default_home: bool,
    /// It authenticates by a stored login ([`ProcessLogin::of_env`]).
    pub stored_login: bool,
}

fn names_another_login(name: &str) -> bool {
    use super::netwatch::{ROUTE_KEYS, ROUTE_PREFIXES};
    CREDENTIAL_KEYS.contains(&name)
        || PROFILE_KEYS.contains(&name)
        || ROUTE_KEYS.contains(&name)
        || ROUTE_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Returns the vendor profile store's `configs` directory for a process with environment `env`.
#[must_use]
pub fn profile_configs_dir<'a>(
    env: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<PathBuf> {
    let (mut named, mut xdg, mut home) = (None, None, None);
    for (name, value) in env {
        let slot = match name {
            "ANTHROPIC_CONFIG_DIR" => &mut named,
            "XDG_CONFIG_HOME" => &mut xdg,
            "HOME" => &mut home,
            _ => continue,
        };
        if !value.trim().is_empty() {
            slot.get_or_insert(value.trim());
        }
    }
    let store = named
        .map(PathBuf::from)
        .or_else(|| xdg.map(|dir| Path::new(dir).join("anthropic")))
        .or_else(|| home.map(|dir| Path::new(dir).join(".config").join("anthropic")))?;
    Some(store.join("configs"))
}

/// Returns whether the vendor profile `configs` directory contains any `.json` profile entry.
#[must_use]
pub fn profile_configured(configs: &Path) -> bool {
    match std::fs::read_dir(configs) {
        Ok(entries) => entries.into_iter().any(|entry| {
            entry.is_ok_and(|e| {
                Path::new(&e.file_name())
                    .extension()
                    .is_some_and(|x| x == "json")
            })
        }),
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

impl ProcessLogin {
    /// Extracts [`ProcessLogin`] facts from `(name, value)` environment pairs.
    #[must_use]
    pub fn of_env<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> ProcessLogin {
        let mut login: Option<&str> = None;
        let mut out = ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        for (name, value) in env {
            if name == LOGIN_ENV {
                match login {
                    None => login = Some(value),
                    Some(first) if first != value => out.stored_login = false,
                    Some(_) => {}
                }
            }
            if value.is_empty() {
                continue;
            }
            if name == HOME_ENV {
                out.default_home = false;
            }
            if names_another_login(name) {
                out.stored_login = false;
            }
        }
        out.login = login.filter(|dir| !dir.is_empty()).map(str::to_owned);
        out
    }

    /// Extracts a `cli` process's [`ProcessLogin`] facts from `(name, value)` environment pairs:
    /// Claude Code's are [`ProcessLogin::of_env`]. Another CLI's login is the directory its seam
    /// variable names; for a [`Seam::LoginFile`], the named file's directory, where the file has
    /// the seam's name (one of another name is no stored login of the pool's). Its home is the
    /// default unless a home variable of its own, other than the seam's, is set. Two different
    /// values of the seam variable, or a credential variable the CLI reads ahead of or in place
    /// of its stored login ([`crate::harness::relaunch::ahead_env`], the ones a pooled line
    /// removes), mean it runs on no stored login. Any non-empty value of such a variable judges
    /// so, even one the CLI ignores and runs on its stored login with (measured on Kimi Code
    /// 2.3.1: a `KIMI_MODEL_API_KEY` with no `KIMI_MODEL_NAME`, a name of only spaces, a
    /// `KIMI_CODE_CUSTOM_HEADERS` with no `Authorization` line), which holds a move rather than
    /// makes one. One read behind the stored login (grok's `XAI_API_KEY`) or that is no login of
    /// the CLI's (`OPENAI_API_KEY` for Codex) judges nothing, so the line that moves a session
    /// and the judge of where it runs agree.
    #[must_use]
    pub fn of_env_for<'a>(
        cli: Cli,
        env: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> ProcessLogin {
        if cli == Cli::Claude {
            return ProcessLogin::of_env(env);
        }
        let connector = cli.connector();
        let seam = cli.accounts().seam;
        let home_var = connector.home.var.filter(|var| *var != seam.var());
        let ahead = super::relaunch::ahead_env(cli);
        let mut login: Option<&str> = None;
        let mut out = ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        for (name, value) in env {
            if name == seam.var() {
                match login {
                    None => login = Some(value),
                    Some(first) if first != value => out.stored_login = false,
                    Some(_) => {}
                }
            }
            if value.is_empty() {
                continue;
            }
            if Some(name) == home_var {
                out.default_home = false;
            }
            if ahead.contains(&name) {
                out.stored_login = false;
            }
        }
        out.login = login.filter(|v| !v.is_empty()).map(|value| match seam {
            Seam::LoginFile { file, .. } => {
                let path = Path::new(value);
                match (path.file_name(), path.parent()) {
                    (Some(name), Some(dir)) if name == file => dir.to_string_lossy().into_owned(),
                    _ => {
                        out.stored_login = false;
                        value.to_owned()
                    }
                }
            }
            Seam::LoginDir { .. } | Seam::Overlay { .. } => value.to_owned(),
        });
        out
    }

    /// Extracts [`ProcessLogin`] facts from a kernel `KEY=value` exec environment slice.
    #[must_use]
    pub fn of_exec_env(env: &[String]) -> Option<ProcessLogin> {
        (!env.is_empty())
            .then(|| ProcessLogin::of_env(env.iter().filter_map(|kv| kv.split_once('='))))
    }

    /// Attaches settings-file credential overrides discovered for the process.
    #[must_use]
    pub fn with_settings(mut self, settings: &[(String, super::netwatch::Source)]) -> ProcessLogin {
        use super::netwatch::{Source, settings_of};
        let another = |source: &Source| match source {
            Source::Missing => false,
            Source::Unreadable => true,
            Source::Text(text) => settings_of(text).is_none_or(|read| {
                read.api_key_helper
                    || read.env.iter().any(|(name, value)| {
                        name == LOGIN_ENV
                            || (!value.is_empty()
                                && (name == HOME_ENV || names_another_login(name)))
                    })
            }),
        };
        self.stored_login &= !settings.iter().any(|(_, source)| another(source));
        self
    }

    /// Attaches whether the vendor profile store holds an active `.json` configuration.
    #[must_use]
    pub const fn with_profile_store(mut self, configured: bool) -> ProcessLogin {
        self.stored_login &= !configured;
        self
    }

    /// Resolves this process against `roster` into a [`Me`] descriptor for [`advise`].
    #[must_use]
    pub fn me<'a>(&self, roster: &'a Roster) -> Me<'a> {
        Me {
            account: roster
                .row_for_login(self.login.as_deref())
                .map(|row| row.label.as_str()),
            left: roster
                .row_left(self.login.as_deref())
                .map(|row| row.label.as_str()),
            default_home: self.default_home,
            stored_login: self.stored_login,
        }
    }
}

/// Returns the roster account label under which `who` runs when no credential override is active.
#[must_use]
pub fn judged_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    let me = who.me(roster);
    me.label().filter(|_| me.default_home && me.stored_login)
}

/// Returns the account label shown in UI surfaces when `roster` lists multiple accounts.
#[must_use]
pub fn footer_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    judged_label(roster, who).filter(|_| roster.several())
}

/// The logins' file name, in the store beside `state.json` ([`State::to_json`]).
pub const LOGINS_FILE: &str = "logins.json";

/// [`LOGINS_FILE`]'s schema.
pub const LOGINS_SCHEMA: u64 = 1;

/// The most bytes of [`LOGINS_FILE`] that are read.
pub const MAX_LOGINS_BYTES: u64 = 256 * 1024;

const DAY_S: i64 = 86_400;

/// A login's lifetime before anything was learned of it.
pub const LIFETIME_START_S: i64 = 21 * DAY_S;

/// The least a learned lifetime is taken as: a week ([`note_login_end`]).
pub const LIFETIME_MIN_S: i64 = 7 * DAY_S;

/// The most a learned lifetime is taken as: the client's own 30 days.
pub const LIFETIME_MAX_S: i64 = 30 * DAY_S;

/// A login whose forecast end is this near is due ([`LoginState::Due`]).
pub const DUE_S: i64 = 5 * DAY_S;

/// A login with a served turn on record this recently is `ok` ([`LoginState::Ok`]).
pub const OK_FRESH_S: i64 = 6 * 3600;

/// The most days the vendor's start-up notice `Your login expires in N days` names.
pub const NOTICE_DAYS_MAX: u32 = 3;

/// The most superseded logins a binding remembers ([`Binding::was`]).
pub const MAX_LEFT: usize = 4;

/// Within this long of now, a machine whose every renewable login is gone or forecast to end has
/// no working login soon ([`none_working_within`]).
pub const NONE_SOON_S: i64 = DAY_S;

const SUBSCRIPTION_MAX: usize = 32;

/// Private evidence left only after a fresh staging sign-in passed its check.
pub(super) const RENEWAL_CHECKPOINT_FILE: &str = ".aterm-renewal.json";
const RENEWAL_CHECKPOINT_MAX: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenewalCheckpoint {
    pub label: String,
    pub email: String,
    pub dir: String,
    pub checked_at: i64,
    pub subscription: Option<String>,
    pub proved_by: Option<Proof>,
}

impl RenewalCheckpoint {
    fn valid(&self) -> bool {
        admissible_label(&self.label)
            && email_refusal(&self.email).is_none()
            && dir_refusal(&self.dir).is_none()
            && self.checked_at > 0
            && self.subscription.as_ref().is_none_or(|s| {
                !s.is_empty() && s.len() <= SUBSCRIPTION_MAX && one_line(s, SUBSCRIPTION_MAX) == *s
            })
    }
}

/// Saves the bounded, private result of a successful fresh sign-in check before binding it.
/// # Errors
/// The scope is invalid, staging cannot be canonicalized, the reserved name already exists, or
/// writing, publishing or flushing the private checkpoint fails.
pub(super) fn write_renewal_checkpoint(
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    checked_at: i64,
) -> io::Result<()> {
    write_renewal_checkpoint_for(
        Cli::Claude,
        label,
        email,
        staging_text,
        subscription,
        None,
        checked_at,
    )
}

/// [`write_renewal_checkpoint`] for a renewal of `cli`'s pool: the checkpoint names the CLI's
/// program, so only that CLI's recovery reads it back ([`read_renewal_checkpoint_for`]).
/// # Errors
/// As [`write_renewal_checkpoint`].
pub(super) fn write_renewal_checkpoint_for(
    cli: Cli,
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    proved_by: Option<Proof>,
    checked_at: i64,
) -> io::Result<()> {
    let staging = Path::new(staging_text);
    std::fs::canonicalize(staging)?;
    let checkpoint = RenewalCheckpoint {
        label: label.to_owned(),
        email: email.to_owned(),
        dir: staging_text.to_owned(),
        checked_at,
        subscription: subscription.map(str::to_owned),
        proved_by,
    };
    if !checkpoint.valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid renewal checkpoint scope",
        ));
    }
    let mut doc = Map::new();
    doc.insert("schema".into(), Value::from(1));
    doc.insert("program".into(), Value::from(cli.id()));
    doc.insert("label".into(), Value::from(checkpoint.label));
    doc.insert("email".into(), Value::from(checkpoint.email));
    doc.insert("dir".into(), Value::from(checkpoint.dir));
    doc.insert("checked_at".into(), Value::from(checkpoint.checked_at));
    doc.insert(
        "subscription".into(),
        checkpoint.subscription.map_or(Value::Null, Value::from),
    );
    if let Some(proof) = checkpoint.proved_by {
        doc.insert("proved_by".into(), Value::from(proof.as_str()));
    }
    let text = Value::Object(doc).to_string();
    if text.len() as u64 > RENEWAL_CHECKPOINT_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "renewal checkpoint exceeds the byte limit",
        ));
    }
    let stage = aterm_tempfile::Builder::new()
        .prefix(".renewal-check-")
        .tempdir_in(staging)?;
    let candidate = stage.path().join("result");
    super::upgrade_models::write_atomic_mode(&candidate, &text, Some(0o600))?;
    std::fs::hard_link(&candidate, staging.join(RENEWAL_CHECKPOINT_FILE))?;
    aterm_durable::durable_dir(staging)
}

/// Reads a bounded regular checkpoint without following a link or waiting on a FIFO.
#[cfg(test)]
#[must_use]
pub(super) fn read_renewal_checkpoint(
    staging: &Path,
    actual_dir: &Path,
    now: i64,
) -> Option<RenewalCheckpoint> {
    read_renewal_checkpoint_for(Cli::Claude, staging, actual_dir, now)
}

/// `read_renewal_checkpoint` of a checkpoint `cli`'s renewal wrote: one naming another CLI's
/// program is not this pool's.
#[must_use]
pub(super) fn read_renewal_checkpoint_for(
    cli: Cli,
    staging: &Path,
    actual_dir: &Path,
    now: i64,
) -> Option<RenewalCheckpoint> {
    use std::io::Read as _;
    let file = astream_sys::open_nonblocking(
        &staging.join(RENEWAL_CHECKPOINT_FILE),
        astream_sys::Links::Refuse,
    )
    .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > RENEWAL_CHECKPOINT_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(RENEWAL_CHECKPOINT_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > RENEWAL_CHECKPOINT_MAX {
        return None;
    }
    let doc: Value = aterm_json::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
    if doc.get("schema")?.as_u64()? != 1 || doc.get("program")?.as_str()? != cli.id() {
        return None;
    }
    let checkpoint = RenewalCheckpoint {
        label: doc.get("label")?.as_str()?.to_owned(),
        email: doc.get("email")?.as_str()?.to_owned(),
        dir: doc.get("dir")?.as_str()?.to_owned(),
        checked_at: doc.get("checked_at")?.as_i64()?,
        subscription: match doc.get("subscription")? {
            Value::Null => None,
            value => Some(value.as_str()?.to_owned()),
        },
        proved_by: match doc.get("proved_by") {
            None | Some(Value::Null) => None,
            Some(value) => Some(Proof::parse(value.as_str()?)?),
        },
    };
    (checkpoint.valid()
        && checkpoint.checked_at <= now
        && std::fs::canonicalize(&checkpoint.dir).ok()?.as_path() == actual_dir)
        .then_some(checkpoint)
}

/// Machine-local login binding and learned renewal lifetime for one roster account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The login directory a renewal bound the row to, verbatim.
    pub dir: Option<String>,
    /// When the bound login was renewed (its staging check answered).
    pub renewed_at: Option<i64>,
    /// The lifetime a renewal is forecast by, as learned ([`Binding::lifetime`],
    /// [`LIFETIME_MIN_S`], [`LIFETIME_MAX_S`]).
    pub lifetime_s: i64,
    /// The latest end the vendor's notice allows, for the bound login ([`note_login_notice`]).
    pub notice_end: Option<i64>,
    /// When the bound login was first seen ended ([`note_login_end`]).
    pub ended_at: Option<i64>,
    /// When a turn was last served under the bound login ([`note_served_login`]).
    pub served_at: Option<i64>,
    /// The `subscriptionType` its renewal's check answered (`max`, `pro`).
    pub subscription: Option<String>,
    /// When a renewal last answered another account than the row's ([`note_wrong_account`]).
    pub wrong_at: Option<i64>,
    /// The logins this binding replaced, newest first, at most [`MAX_LEFT`].
    pub was: Vec<Option<String>>,
    /// What proved the bound login is the row's account, where a sign-in of another CLI's pool
    /// bound it ([`note_proof`]); a Claude Code renewal's proof is its status check, never
    /// written.
    pub proved_by: Option<Proof>,
    /// The remaining human step of a renewal, cleared by a successful sign-in.
    pub sign_in_task: Option<SignInTask>,
}

/// The human step a vendor renewal could not finish, without its page or error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignInAction {
    /// Sign the row's browser profile in once, then retry its renewal.
    Browser,
    /// Approve the CLI's own sign-in on a terminal.
    Login,
}

impl SignInAction {
    /// The stored action word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Login => "login",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "browser" => Some(Self::Browser),
            "login" => Some(Self::Login),
            _ => None,
        }
    }

    /// The next step for this action on `label` in `cli`'s pool.
    #[must_use]
    pub fn next_step(self, cli: Cli, label: &str) -> String {
        let id = cli.id();
        match self {
            Self::Browser => format!(
                "Run aterm harness accounts browser {label} --cli {id}, then aterm harness \
                 accounts renew {label} --cli {id}"
            ),
            Self::Login => format!("Run aterm harness accounts login {label} --cli {id}"),
        }
    }
}

/// A durable renewal task. Its timestamp is evidence, never a login identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInTask {
    /// The remaining step.
    pub action: SignInAction,
    /// When the renewal stopped for a person, in Unix seconds.
    pub at: i64,
}

/// What proved a sign-in made the row's account ([`Binding::proved_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// The CLI's own report named the row's email (Codex's `account/read`, Grok's `Signed in
    /// as`).
    Status,
    /// The sign-in page showed the row's email.
    Page,
    /// Nothing named the account: the person approved the page.
    Unchecked,
}

impl Proof {
    /// Its `logins.json` word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Proof::Status => "status",
            Proof::Page => "page",
            Proof::Unchecked => "unchecked",
        }
    }

    /// The proof its `logins.json` `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Proof> {
        [Proof::Status, Proof::Page, Proof::Unchecked]
            .into_iter()
            .find(|p| p.as_str() == word)
    }
}

impl Default for Binding {
    fn default() -> Binding {
        Binding {
            dir: None,
            renewed_at: None,
            lifetime_s: LIFETIME_START_S,
            notice_end: None,
            ended_at: None,
            served_at: None,
            subscription: None,
            wrong_at: None,
            was: Vec::new(),
            proved_by: None,
            sign_in_task: None,
        }
    }
}

impl Binding {
    /// Clamped login lifetime in seconds used to forecast expiration.
    #[must_use]
    pub fn lifetime(&self) -> i64 {
        self.lifetime_s.clamp(LIFETIME_MIN_S, LIFETIME_MAX_S)
    }

    fn lower_lifetime(&mut self, end: i64) {
        if let Some(renewed) = self.renewed_at
            && end > renewed
        {
            let implied = end.saturating_sub(renewed);
            self.lifetime_s = self
                .lifetime_s
                .min(implied)
                .clamp(LIFETIME_MIN_S, LIFETIME_MAX_S);
        }
    }
}

/// Machine-local `logins.json` map from account label to [`Binding`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Logins {
    /// The bindings, by label.
    pub labels: BTreeMap<String, Binding>,
}

impl Logins {
    /// Looks up the [`Binding`] for `label`.
    #[must_use]
    pub fn get(&self, label: &str) -> Option<&Binding> {
        self.labels.get(label)
    }

    fn entry(&mut self, label: &str) -> Option<&mut Binding> {
        if !admissible_label(label)
            || (!self.labels.contains_key(label) && self.labels.len() >= MAX_ACCOUNTS)
        {
            return None;
        }
        Some(self.labels.entry(label.to_owned()).or_default())
    }

    /// Serializes `logins.json` as a single-line sorted JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(LOGINS_SCHEMA));
        let labels = self
            .labels
            .iter()
            .map(|(label, b)| {
                let mut m = Map::new();
                m.insert("dir".to_owned(), Value::from(b.dir.clone()));
                m.insert("renewed_at".to_owned(), Value::from(b.renewed_at));
                m.insert("lifetime_s".to_owned(), Value::from(b.lifetime_s));
                m.insert("notice_end".to_owned(), Value::from(b.notice_end));
                m.insert("ended_at".to_owned(), Value::from(b.ended_at));
                m.insert("served_at".to_owned(), Value::from(b.served_at));
                m.insert(
                    "subscription".to_owned(),
                    Value::from(b.subscription.clone()),
                );
                m.insert("wrong_at".to_owned(), Value::from(b.wrong_at));
                m.insert(
                    "was".to_owned(),
                    Value::Array(b.was.iter().map(|w| Value::from(w.clone())).collect()),
                );
                if let Some(proof) = b.proved_by {
                    m.insert("proved_by".to_owned(), Value::from(proof.as_str()));
                }
                if let Some(task) = b.sign_in_task {
                    let mut pending = Map::new();
                    pending.insert("action".into(), Value::from(task.action.as_str()));
                    pending.insert("at".into(), Value::from(task.at));
                    m.insert("sign_in_task".into(), Value::Object(pending));
                }
                (label.clone(), Value::Object(m))
            })
            .collect();
        o.insert("labels".to_owned(), Value::Object(labels));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Deserializes and validates `logins.json` against `now`.
    /// # Errors
    ///
    /// As [`State::from_json`]: [`Unread::Torn`] for text that is not JSON,
    /// [`Unread::NotOurs`] for another schema or invalid identity structure.
    pub fn from_json(text: &str, now: i64) -> Result<Logins, Unread> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|e| Unread::Torn(format!("not JSON ({})", shown(&e.to_string()))))?;
        if !doc.is_object() {
            return Err(Unread::NotOurs("not a JSON object".to_string()));
        }
        match doc.get("schema").and_then(Value::as_u64) {
            Some(LOGINS_SCHEMA) => {}
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schema {LOGINS_SCHEMA}"
                )));
            }
            None => return Err(Unread::NotOurs("names no schema".to_string())),
        }
        let entries = doc
            .get("labels")
            .and_then(Value::as_object)
            .ok_or_else(|| Unread::NotOurs("labels is not an object".to_string()))?;
        if entries.len() > MAX_ACCOUNTS {
            return Err(Unread::NotOurs("too many binding labels".to_string()));
        }
        let mut labels = BTreeMap::new();
        for (label, value) in entries {
            if !admissible_label(label) || !value.is_object() {
                return Err(Unread::NotOurs(
                    "invalid binding label or object".to_string(),
                ));
            }
            let dir_of = |value: &Value| match value {
                Value::Null => Ok(None),
                Value::String(dir) if dir_refusal(dir).is_none() => Ok(Some(dir.clone())),
                _ => Err(Unread::NotOurs(format!(
                    "binding `{label}` has an invalid login directory"
                ))),
            };
            let dir = dir_of(value.get("dir").ok_or_else(|| {
                Unread::NotOurs(format!("binding `{label}` names no login directory"))
            })?)?;
            let was = match value.get("was") {
                Some(Value::Array(was)) if was.len() <= MAX_LEFT => {
                    was.iter().map(dir_of).collect::<Result<Vec<_>, _>>()?
                }
                _ => {
                    return Err(Unread::NotOurs(format!(
                        "binding `{label}` has invalid superseded logins"
                    )));
                }
            };
            let v = value;
            let sign_in_task = match v.get("sign_in_task") {
                None | Some(Value::Null) => None,
                Some(task) => {
                    let action = task
                        .get("action")
                        .and_then(Value::as_str)
                        .and_then(SignInAction::parse)
                        .ok_or_else(|| {
                            Unread::NotOurs(format!(
                                "binding `{label}` has an unknown sign-in task"
                            ))
                        })?;
                    let at = stamp(task.get("at"), now).ok_or_else(|| {
                        Unread::NotOurs(format!("binding `{label}` has no sign-in task timestamp"))
                    })?;
                    Some(SignInTask { action, at })
                }
            };
            let binding = Binding {
                dir,
                renewed_at: stamp(v.get("renewed_at"), now),
                lifetime_s: int(v.get("lifetime_s"))
                    .unwrap_or(LIFETIME_START_S)
                    .clamp(LIFETIME_MIN_S, LIFETIME_MAX_S),
                notice_end: int(v.get("notice_end")),
                ended_at: stamp(v.get("ended_at"), now),
                served_at: stamp(v.get("served_at"), now),
                subscription: word(v.get("subscription"), SUBSCRIPTION_MAX),
                wrong_at: stamp(v.get("wrong_at"), now),
                was,
                proved_by: v
                    .get("proved_by")
                    .and_then(Value::as_str)
                    .and_then(Proof::parse),
                sign_in_task,
            };
            labels.insert(label.clone(), binding);
        }
        Ok(Logins { labels })
    }
}

/// Applies machine-local renewal bindings in `logins` to `roster`.
#[must_use]
pub fn bound(roster: &Roster, logins: &Logins) -> Roster {
    let rows = roster
        .rows
        .iter()
        .map(|row| match logins.get(&row.label) {
            Some(b) if b.dir.is_some() => {
                let mut was = b.was.clone();
                if row.dir.is_none() && !was.contains(&None) {
                    was.push(None);
                }
                Row {
                    dir: b.dir.clone(),
                    was,
                    ..row.clone()
                }
            }
            _ => row.clone(),
        })
        .collect();
    Roster { rows }
}

/// Derived renewal state of a roster account's login on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// A login wall was seen on it ([`Auth::SignedOut`] on record).
    Expired,
    /// Its forecast end ([`forecast`]) is within [`DUE_S`].
    Due {
        /// The forecast end.
        by: i64,
    },
    /// A turn was served under it within [`OK_FRESH_S`].
    Ok,
    /// None of the above: not a fault.
    Idle,
}

impl LoginState {
    /// Short status word for reports and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LoginState::Expired => "expired",
            LoginState::Due { .. } => "due",
            LoginState::Ok => "ok",
            LoginState::Idle => "idle",
        }
    }
}

/// Forecasts the Unix expiration timestamp of `binding`.
#[must_use]
pub fn forecast(binding: &Binding) -> Option<i64> {
    let by_life = binding
        .renewed_at
        .map(|at| at.saturating_add(binding.lifetime()));
    [by_life, binding.notice_end].into_iter().flatten().min()
}

/// Computes the [`LoginState`] of `row` at `now` from `logins` and `state`.
#[must_use]
pub fn login_state(row: &Row, logins: &Logins, state: &State, now: i64) -> LoginState {
    if state
        .accounts
        .get(&row.label)
        .is_some_and(|a| a.auth == Auth::SignedOut)
    {
        return LoginState::Expired;
    }
    let binding = logins.get(&row.label);
    if let Some(by) = binding.and_then(forecast)
        && by.saturating_sub(now) <= DUE_S
    {
        return LoginState::Due { by };
    }
    if binding
        .and_then(|b| b.served_at)
        .is_some_and(|at| recent(at, now, OK_FRESH_S))
    {
        LoginState::Ok
    } else {
        LoginState::Idle
    }
}

/// Returns the labels of accounts with an `email` whose login is expired or due for renewal.
#[must_use]
pub fn renew_due(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| row.email.is_some() && is_enrolled(row, enrolled))
        .filter(|row| {
            matches!(
                login_state(row, logins, state, now),
                LoginState::Expired | LoginState::Due { .. }
            )
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Returns whether the roster has renewable accounts enrolled here and none remain working past
/// `now + within`.
#[must_use]
pub fn none_working_within(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
    within: i64,
) -> bool {
    let mut rows = roster
        .rows
        .iter()
        .filter(|row| row.email.is_some() && is_enrolled(row, enrolled))
        .peekable();
    rows.peek().is_some()
        && rows.all(|row| match login_state(row, logins, state, now) {
            LoginState::Expired => true,
            _ => logins
                .get(&row.label)
                .and_then(forecast)
                .is_some_and(|by| by.saturating_sub(now) <= within),
        })
}

/// Returns the labels in roster order whose last renewal authenticated the wrong account.
#[must_use]
pub fn wrong_accounts(roster: &Roster, logins: &Logins) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| logins.get(&row.label).is_some_and(|b| b.wrong_at.is_some()))
        .map(|row| row.label.clone())
        .collect()
}

/// The roster's pending human sign-in actions, in roster order.
#[must_use]
pub fn sign_in_tasks(roster: &Roster, logins: &Logins) -> Vec<(String, SignInAction)> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let task = logins.get(&row.label)?.sign_in_task?;
            Some((row.label.clone(), task.action))
        })
        .collect()
}

/// Records a renewal's remaining human action without changing its login binding.
pub fn note_sign_in_task(logins: &mut Logins, label: &str, action: SignInAction, at: i64) {
    if let Some(binding) = logins.entry(label) {
        binding.sign_in_task = Some(SignInTask { action, at });
    }
}

/// Records a login wall hit at `at` on `label`'s bound login and updates learned lifetime.
pub fn note_login_end(state: &mut State, logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    let mut out = if state.account(label).auth == Auth::SignedOut {
        Vec::new()
    } else {
        note_auth(state, label, Auth::SignedOut, at)
    };
    let Some(binding) = logins.entry(label) else {
        return out;
    };
    let first = binding
        .ended_at
        .is_none_or(|ended| binding.renewed_at.is_some_and(|renewed| ended < renewed));
    if first {
        binding.ended_at = Some(at);
        if binding
            .renewed_at
            .is_some_and(|renewed| at.saturating_sub(renewed) >= LIFETIME_MIN_S)
        {
            binding.lower_lifetime(at);
        }
        out.push(Event::new(at, Kind::LoginEnded, label));
    }
    out
}

/// Records a vendor `Your login expires in N days` notice read at `at` on `label`.
pub fn note_login_notice(logins: &mut Logins, label: &str, days: u32, at: i64) -> bool {
    if days == 0 || days > NOTICE_DAYS_MAX {
        return false;
    }
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let end = at.saturating_add(i64::from(days).saturating_mul(DAY_S));
    let before = binding.clone();
    binding.notice_end = Some(binding.notice_end.map_or(end, |held| held.min(end)));
    binding.lower_lifetime(end);
    *binding != before
}

/// Records that `label`'s bound login served a turn at `at`.
pub fn note_served_login(logins: &mut Logins, label: &str, at: i64) -> bool {
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let served = binding.served_at.map_or(at, |held| held.max(at));
    let changed = binding.served_at != Some(served);
    binding.served_at = Some(served);
    changed
}

/// Records that `label`'s bound login served a turn at `at` after being marked signed out.
pub fn note_signed_back(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    at: i64,
) -> Vec<Event> {
    if state
        .accounts
        .get(label)
        .is_none_or(|a| a.auth != Auth::SignedOut)
    {
        return Vec::new();
    }
    let out = note_auth(state, label, Auth::Ok, at);
    if out.is_empty() {
        return out;
    }
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.renewed_at = Some(at);
        binding.notice_end = None;
        binding.served_at = Some(binding.served_at.map_or(at, |held| held.max(at)));
    }
    out
}

/// Binds `label` to the newly authenticated staging directory `dir` after a verified renewal.
pub fn note_renewed(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    own_dir: Option<&str>,
    dir: &str,
    subscription: Option<&str>,
    at: i64,
) -> Vec<Event> {
    let mut out = Vec::new();
    let Some(binding) = logins.entry(label) else {
        return out;
    };
    let replaced = binding.dir.clone().or_else(|| own_dir.map(str::to_owned));
    binding
        .was
        .retain(|was| *was != replaced && was.as_deref() != Some(dir));
    binding.was.insert(0, replaced);
    binding.was.truncate(MAX_LEFT);
    binding.dir = Some(dir.to_owned());
    binding.renewed_at = Some(at);
    binding.notice_end = None;
    binding.ended_at = None;
    binding.wrong_at = None;
    binding.proved_by = None;
    binding.sign_in_task = None;
    binding.subscription = subscription
        .map(|s| one_line(s, SUBSCRIPTION_MAX))
        .filter(|s| !s.is_empty());
    out.extend(note_auth(state, label, Auth::Ok, at));
    out.push(Event::new(at, Kind::Renewed, label));
    out
}

/// Records what proved `label`'s newly bound login is its account ([`Binding::proved_by`]),
/// after [`note_renewed`] in the same transaction.
pub fn note_proof(logins: &mut Logins, label: &str, proof: Proof) {
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.proved_by = Some(proof);
    }
}

/// Records that a renewal attempt for `label` at `at` authenticated a different account.
pub fn note_wrong_account(logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    match logins.entry(label) {
        Some(binding) => {
            binding.wrong_at = Some(at);
            binding.sign_in_task = None;
            vec![Event::new(at, Kind::WrongAccount, label)]
        }
        None => Vec::new(),
    }
}

/// Newest observation or scoped clearing of a single vendor quota-limit type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wall {
    /// The reset the wall named (Unix seconds), or `None` when it named none.
    pub until: Option<i64>,
    /// When it was observed.
    pub at: i64,
    /// The machine that observed it.
    pub by: String,
    /// How many times it was found standing with no reset ahead of it.
    pub attempts: u32,
    /// This window was cleared at `at`, rather than observed standing.
    pub cleared: bool,
}

fn rung(ladder: &[i64], attempts: u32) -> i64 {
    let i = usize::try_from(attempts.saturating_sub(1)).unwrap_or(usize::MAX);
    ladder
        .get(i)
        .or_else(|| ladder.last())
        .copied()
        .unwrap_or(0)
}

impl Wall {
    /// Returns the Unix timestamp until which this observed wall remains standing.
    #[must_use]
    pub fn stands_until(&self) -> Option<i64> {
        if self.cleared {
            return None;
        }
        Some(match self.until {
            Some(until) if self.at <= until => until.saturating_add(RESET_GRACE_S),
            Some(_) => self.at.saturating_add(rung(&LATE_WALL_S, self.attempts)),
            None => self
                .at
                .saturating_add(rung(&NO_RESET_WALL_S, self.attempts)),
        })
    }
}

/// An account's usage sample: a settled `/usage` panel, or the figures held by its tap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sample {
    /// `Current session`, whole percent used.
    pub session_pct: Option<u32>,
    /// When the session window resets.
    pub session_reset: Option<i64>,
    /// `Current week (all models)`, whole percent used.
    pub week_pct: Option<u32>,
    /// When the week resets.
    pub week_reset: Option<i64>,
    /// When the session figure was observed. Absent in older records: use [`Self::at`].
    pub session_read_at: Option<i64>,
    /// When the weekly figure was observed: a tap heartbeat keeps the window's time, a settled
    /// `/usage` panel its own read time. Absent in older records: use [`Self::at`].
    pub week_read_at: Option<i64>,
    /// When the sample was published, for heartbeat and probe scheduling. A tap sample's
    /// individual figures can have been observed earlier.
    pub at: i64,
    /// The machine that read it ([`note_sample`] stamps it).
    pub by: String,
}

fn figure(
    pct: Option<u32>,
    reset: Option<i64>,
    at: i64,
    window_s: i64,
    now: i64,
) -> Option<(u32, i64)> {
    let end = reset.unwrap_or_else(|| at.saturating_add(window_s));
    pct.filter(|_| now < end).map(|pct| (pct, end))
}

impl Sample {
    /// Returns the 5-hour session utilization percentage and reset timestamp while the window is
    /// active.
    #[must_use]
    pub fn session(&self, now: i64) -> Option<(u32, i64)> {
        figure(
            self.session_pct,
            self.session_reset,
            self.at,
            SESSION_WINDOW_S,
            now,
        )
    }

    /// Returns the 7-day weekly utilization percentage and reset timestamp while the window is
    /// active.
    #[must_use]
    pub fn week(&self, now: i64) -> Option<(u32, i64)> {
        figure(self.week_pct, self.week_reset, self.at, WEEK_WINDOW_S, now)
    }

    /// Returns the latest timestamp until which any running window in this sample is at or above
    /// its low threshold.
    #[must_use]
    pub fn low_until(&self, now: i64) -> Option<i64> {
        let at = |figure: Option<(u32, i64)>, threshold: u32| {
            figure
                .filter(|(pct, _)| *pct >= threshold)
                .map(|(_, end)| end)
        };
        at(self.session(now), LOW_SESSION_PCT).max(at(self.week(now), LOW_WEEK_PCT))
    }

    /// Returns whether any running window in this sample is at or above its low threshold at `now`.
    #[must_use]
    pub fn low(&self, now: i64) -> bool {
        self.low_until(now).is_some()
    }

    /// Returns whether a session on this account should rotate early before hitting a wall.
    #[must_use]
    pub fn leave_early(&self, now: i64) -> bool {
        self.low_until(now)
            .is_some_and(|end| end.saturating_sub(now) > RESET_SOON_S)
    }
}

/// Limit-reset offer observed at a usage wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The kind `left` counts, where one was read.
    pub kind: Option<ResetKind>,
    /// Resets left (of `kind`, where one is named).
    pub left: u32,
    /// The grant's deadline, where one was read.
    pub use_by: Option<i64>,
    /// When it was seen.
    pub at: i64,
    /// The machine that saw it.
    pub by: String,
}

/// Newest observation of whether usage credits are carrying an account (`InUse` or `Off`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credits {
    /// Seen carrying the account's sessions past a limit.
    InUse(CreditsUse),
    /// Off, as a limit row said (`quotaLimits.overageStatus: rejected`).
    Off(CreditsOff),
    /// On, as a limit row said (`allowed`, `allowed_warning`), at this instant.
    On(i64),
}

impl Credits {
    /// Timestamp when this credits observation was recorded.
    #[must_use]
    pub const fn at(&self) -> i64 {
        match self {
            Credits::InUse(c) => c.seen,
            Credits::Off(off) => off.at,
            Credits::On(at) => *at,
        }
    }
}

/// Active usage-credits episode carrying an account's sessions past a limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsUse {
    /// The first answer seen served on credits, this episode.
    pub since: i64,
    /// The newest.
    pub seen: i64,
    /// The reset of the window credits carried past, where the notice they started on named it.
    pub until: Option<i64>,
    /// This episode raised the day's line ([`credits_noted`]).
    pub told: bool,
}

impl CreditsUse {
    /// Returns whether this credits episode is still considered active at `now`.
    #[must_use]
    pub fn stands(&self, now: i64) -> bool {
        now < self.until_at()
    }

    /// Timestamp until which this credits episode is considered active.
    #[must_use]
    pub fn until_at(&self) -> i64 {
        let reset = self
            .until
            .unwrap_or_else(|| self.since.saturating_add(SESSION_WINDOW_S));
        reset.max(self.seen.saturating_add(CREDITS_STALE_S))
    }
}

/// Observation that an account's usage credits are disabled, with the vendor's reason word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsOff {
    /// The vendor's word for why.
    pub reason: String,
    /// When the row was written.
    pub at: i64,
    /// When they come back (`overageResetsAt`), where the row named it.
    pub until: Option<i64>,
}

/// Warning utilization figure and reset timestamp read from a session screen outside `/usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSaid {
    /// Whole percent used, as of [`Self::at`].
    pub pct: Option<u32>,
    /// When the window resets, where the warning named it ([`reset_outcome`]).
    pub reset: Option<i64>,
    /// When it was true.
    pub at: i64,
}

impl WindowSaid {
    /// Returns the utilization percentage and window end timestamp while the warning window is
    /// active.
    #[must_use]
    pub fn figure(&self, window_s: i64, now: i64) -> Option<(u32, i64)> {
        figure(self.pct, self.reset, self.at, window_s, now)
    }
}

/// [`WindowFig::source`] of a usage tap's reading ([`note_reading`]).
pub const SOURCE_TAP: &str = "tap";

/// [`WindowFig::source`] of a settled `/usage` panel figure from the account's [`Sample`]
/// ([`note_sample`]).
pub const SOURCE_SAMPLE: &str = "sample";

/// The most windows one account keeps a [`WindowFig`] for: the vendor's two, its per-model weeks
/// and its spend limit, with room.
pub const MAX_WINDOWS: usize = 8;

/// A drop smaller than this, in tenths of a percent, is no refill ([`WindowRead::refilled`]).
pub const REFILL_MIN_X10: u32 = 10;

/// Single quota-window utilization reading held on an [`Account`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFig {
    /// Tenths of a percent used (`425` is 42.5%), at most `10 *` [`PCT_MAX`].
    pub pct_x10: u32,
    /// The window's reset, the vendor's instant ([`merge_fig`]).
    pub reset: i64,
    /// When the reading was served, as near as its source knows ([`TapRead::at`]).
    pub at: i64,
    /// The machine that read it.
    pub by: String,
    /// Where it came from: in the state, [`SOURCE_TAP`] or [`SOURCE_SAMPLE`].
    pub source: String,
    /// A limit reset used inside this window ([`WindowRead::refilled`], [`reset_outcome`]).
    pub epoch_at: Option<i64>,
}

impl WindowFig {
    /// Returns whether this window reading's reset timestamp is still in the future at `now`.
    #[must_use]
    pub const fn current(&self, now: i64) -> bool {
        now < self.reset
    }

    /// Returns whether this reading was served before an in-window limit reset.
    #[must_use]
    pub fn cut(&self) -> bool {
        self.epoch_at.is_some_and(|epoch| self.at < epoch)
    }

    /// Utilization percentage rounded half-up to a whole integer.
    #[must_use]
    pub const fn whole(&self) -> u32 {
        self.pct_x10.saturating_add(5) / 10
    }
}

/// Merges a stored [`WindowFig`] with an incoming reading for the same quota window.
#[must_use]
pub fn merge_fig(
    held: Option<&WindowFig>,
    read: Option<&WindowFig>,
    now: i64,
) -> Option<WindowFig> {
    let held = held.filter(|h| h.current(now));
    let (h, r) = match (held, read) {
        (None, r) => return r.cloned(),
        (h, None) => return h.cloned(),
        (Some(h), Some(r)) => (h, r),
    };
    if r.reset != h.reset {
        return Some(if r.reset > h.reset { r } else { h }.clone());
    }
    let epoch_at = h.epoch_at.max(r.epoch_at);
    let live = |f: &WindowFig| epoch_at.is_none_or(|epoch| f.at >= epoch);
    let pick = match (live(h), live(r)) {
        (true, true) if (r.pct_x10, r.at) > (h.pct_x10, h.at) => r,
        (false, false) if r.at > h.at => r,
        (true, true) | (true, false) | (false, false) => h,
        (false, true) => r,
    };
    Some(WindowFig {
        epoch_at,
        ..pick.clone()
    })
}

/// Single quota window from a usage-tap reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRead {
    /// Tenths of a percent used, as the vendor sent it (`used_percentage` carries one decimal).
    pub pct_x10: u32,
    /// The window's reset.
    pub reset: i64,
    /// The figure the vendor refilled the window from, where it did ([`REFILL_MIN_X10`],
    /// [`TapRead::seen_before`]).
    pub refilled: Option<u32>,
}

/// Fresh usage-tap reading passed to [`note_reading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TapRead {
    /// The five-hour window, where the reading carried it.
    pub five_hour: Option<WindowRead>,
    /// The week.
    pub seven_day: Option<WindowRead>,
    /// When its figures were served.
    pub at: i64,
    /// The host read this process's tap before and the process was served since.
    pub seen_before: bool,
    /// When the process began (its kernel start, Unix seconds), where the host read it.
    pub born: Option<i64>,
    /// The account's key.
    pub key: Option<super::profile::AccountKey>,
}

/// Machine-local authentication status last observed for an account's login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Auth {
    /// Nothing is on record.
    #[default]
    Unseen,
    /// The check after a sign-in, or a served response, said the login works.
    Ok,
    /// A login wall was seen on it ([`strike`], [`note_login_end`]), a first login's flow
    /// failed, or the check after a sign-in said it is signed out.
    SignedOut,
}

impl Auth {
    /// Canonical status word for `state.json` and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Auth::Unseen => "unseen",
            Auth::Ok => "ok",
            Auth::SignedOut => "signed-out",
        }
    }

    /// Parses an [`Auth`] status from its state-file word ([`Auth::Unseen`]).
    #[must_use]
    pub fn parse(word: &str) -> Auth {
        match word {
            "ok" => Auth::Ok,
            "signed-out" => Auth::SignedOut,
            _ => Auth::Unseen,
        }
    }
}

/// Per-account observations persisted in `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    /// Observation: the walls, by the vendor's limit type.
    pub walls: BTreeMap<String, Wall>,
    /// Observation: the newest sample.
    pub sample: Option<Sample>,
    /// Observation: the sample before it (for the burn rate).
    pub before: Option<Sample>,
    /// Observation: the reset offer last seen.
    pub offer: Option<Offer>,
    /// Machine-local: the login's last known word.
    pub auth: Auth,
    /// Machine-local: when that word was learned.
    pub auth_at: Option<i64>,
    /// Machine-local: the latest account-wide clearing ([`pin`], [`note_reading`]).
    pub cleared_at: Option<i64>,
    /// Machine-local: when a session was last read running on it ([`note_session`],
    /// [`reset_allowed`]).
    pub ran_at: Option<i64>,
    /// Observation: the newest word on its usage credits ([`Credits`]).
    pub credits: Option<Credits>,
    /// Machine-local: the local day the last credits episode that raised the day's line began on.
    pub credits_day: Option<i64>,
    /// Observation: the newest word on its five-hour window apart from a `/usage` sample
    /// ([`WindowSaid`]).
    pub warned_session: Option<WindowSaid>,
    /// Observation: the same of its week ([`windows_full`], [`Self::week_low_until`]).
    pub warned_week: Option<WindowSaid>,
    /// Observation: the account this label's readings were last read of, by its key.
    pub key: Option<super::profile::AccountKey>,
    /// Machine-local: when a reading under another key than [`Self::key`] held was recorded
    /// ([`note_reading`], [`Kind::Epoch`]).
    pub keyed_at: Option<i64>,
    /// Observation: each window's figure ([`WindowFig`], [`Self::sample`]).
    pub windows: BTreeMap<String, WindowFig>,
}

impl Account {
    /// Returns whether a reading at `at` comes before the end of the [`KEY_ADOPT_S`] grace that
    /// follows a reading under another key ([`Self::keyed_at`]).
    #[must_use]
    pub fn adopting(&self, at: i64) -> bool {
        self.keyed_at
            .is_some_and(|keyed| at < keyed.saturating_add(KEY_ADOPT_S))
    }

    /// Returns whether any wall, sample, offer, auth word, credits word, warning or window figure
    /// is on record.
    #[must_use]
    pub fn observed(&self) -> bool {
        !self.walls.is_empty()
            || self.sample.is_some()
            || self.offer.is_some()
            || self.auth != Auth::Unseen
            || self.credits.is_some()
            || self.warned_session.is_some()
            || self.warned_week.is_some()
            || !self.windows.is_empty()
    }

    /// Returns whether this account's recorded [`super::profile::AccountKey`] differs from `key`.
    #[must_use]
    pub fn of_another(&self, key: Option<super::profile::AccountKey>) -> bool {
        matches!((self.key, key), (Some(held), Some(key)) if held != key)
    }

    /// Returns the latest timestamp until which a standing wall blocks this account at `now`.
    #[must_use]
    pub fn limited_until(&self, now: i64) -> Option<i64> {
        self.walls
            .values()
            .filter_map(Wall::stands_until)
            .filter(|end| now < *end)
            .max()
    }

    /// Returns the timestamp until which the 5-hour session window marks this account `low` at
    /// `now`.
    #[must_use]
    pub fn session_low_until(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_session
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.session(now))
            .flatten()
            .filter(|(pct, _)| *pct >= LOW_SESSION_PCT)
            .map(|(_, end)| end);
        let warned = self
            .warned_session
            .as_ref()
            .and_then(|w| w.figure(SESSION_WINDOW_S, now))
            .filter(|(pct, _)| *pct >= LOW_SESSION_PCT)
            .map(|(_, end)| end);
        sample.max(warned)
    }

    /// Returns the timestamp until which the 7-day weekly window marks this account `low` at `now`.
    #[must_use]
    pub fn week_low_until(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_week
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.week(now))
            .flatten()
            .filter(|(pct, _)| *pct >= LOW_WEEK_PCT)
            .map(|(_, end)| end);
        let warned = self
            .warned_week
            .as_ref()
            .and_then(|w| w.figure(WEEK_WINDOW_S, now))
            .filter(|(pct, _)| *pct >= LOW_WEEK_PCT)
            .map(|(_, end)| end);
        sample.max(warned)
    }

    /// Returns the timestamp until which active usage credits mark this account `low` at `now`.
    #[must_use]
    pub fn credits_until(&self, now: i64) -> Option<i64> {
        match &self.credits {
            Some(Credits::InUse(c)) if c.stands(now) => Some(c.until_at()),
            _ => None,
        }
    }

    /// Returns the latest timestamp until which this account is `low` at `now`.
    #[must_use]
    pub fn low_until(&self, now: i64) -> Option<i64> {
        [
            self.session_low_until(now),
            self.week_low_until(now),
            self.credits_until(now),
        ]
        .into_iter()
        .flatten()
        .max()
    }

    /// Returns whether this account is `low` at `now`.
    #[must_use]
    pub fn low(&self, now: i64) -> bool {
        self.low_until(now).is_some()
    }

    /// Returns whether a session on this account should proactively rotate off before hitting a
    /// wall.
    #[must_use]
    pub fn leave_early(&self, now: i64) -> bool {
        self.credits_until(now).is_some()
            || self
                .low_until(now)
                .is_some_and(|end| end.saturating_sub(now) > RESET_SOON_S)
    }

    /// Returns the 7-day weekly reset timestamp for this account if known and in the future at
    /// `now`.
    #[must_use]
    pub fn week_reset(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_week
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.week_reset)
            .flatten()
            .filter(|reset| *reset > now);
        let warned = self
            .warned_week
            .as_ref()
            .and_then(|w| w.figure(WEEK_WINDOW_S, now))
            .and_then(|(_, _)| self.warned_week.as_ref()?.reset)
            .filter(|reset| *reset > now);
        sample.or(warned)
    }

    /// Returns the timestamp when `window` was cleared after `at` by a scoped reset or operator
    /// pin.
    #[must_use]
    pub fn cleared_since(&self, window: &str, at: i64, now: i64) -> Option<i64> {
        self.cleared_at
            .into_iter()
            .chain(
                self.walls
                    .get(window)
                    .filter(|wall| wall.cleared)
                    .map(|wall| wall.at),
            )
            .map(|cleared| cleared.min(now))
            .filter(|cleared| at < *cleared)
            .max()
    }

    fn touched(&self) -> i64 {
        self.walls
            .values()
            .map(|w| w.at)
            .chain(self.sample.as_ref().map(|s| s.at))
            .chain(self.offer.as_ref().map(|o| o.at))
            .chain(self.auth_at)
            .chain(self.cleared_at)
            .chain(self.keyed_at)
            .chain(self.ran_at)
            .chain(self.credits.as_ref().map(Credits::at))
            .chain(self.warned_session.as_ref().map(|w| w.at))
            .chain(self.warned_week.as_ref().map(|w| w.at))
            .chain(self.windows.values().map(|w| w.at))
            .max()
            .unwrap_or(i64::MIN)
    }
}

/// Machine-local record of which account a supervised session is running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The account its process's own environment names.
    pub account: String,
    /// When that was last read.
    pub at: i64,
    /// Why it could not move, where a move was refused.
    pub held: Option<String>,
}

/// Machine-local claim for an in-flight or recently completed `/usage` probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeClaim {
    /// The account probed.
    pub account: String,
    /// The session that types it.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
}

/// Machine-local canary record holding other sessions back until a newly rotated account serves a
/// response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canary {
    /// The session that moved alone.
    pub sid: String,
    /// When it did: the rotation's time, or the time it took a silent canary's place.
    pub at: i64,
    /// It took the place of a canary that did not answer ([`take_canary`], [`Strike::NotMoved`]).
    pub took: bool,
}

/// Rollback snapshot saved by [`begin_rotation`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Undo {
    /// [`State::previous`] as the rotation found it.
    pub previous: Option<String>,
    /// [`State::previous_proven`] as the rotation found it.
    pub previous_proven: bool,
    /// [`State::canary`] as the rotation found it.
    pub canary: Option<Canary>,
}

/// Machine-local claim for a single limit-reset attempt on a standing wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetClaim {
    /// The account at the wall.
    pub account: String,
    /// The wall's own reset.
    pub until: Option<i64>,
    /// The vendor's raw limit type observed at this claim.
    pub window: Option<String>,
    /// The session that takes the row.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
    /// The pid of the process the claim was made for.
    pub pid: u32,
    /// That process's kernel start, as the host reads it.
    pub start: String,
    /// The outcome's word once it was recorded ([`reset_outcome`], [`ResetDone::word`]).
    pub done: Option<String>,
}

/// An explicit account selection awaiting a verified login on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRequest {
    label: String,
    nonce: String,
}

impl PinRequest {
    fn new(label: &str, nonce: String) -> Option<Self> {
        (label_refusal(label).is_none() && aterm_codec::hex::is_hex(&nonce, 32)).then(|| Self {
            label: label.to_owned(),
            nonce,
        })
    }

    /// The canonical account label selected before its login worker started.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Persisted accounts state (`state.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// [`SCHEMA`].
    pub schema: u64,
    /// Machine-local: this machine's id ([`Store::machine_id`]).
    pub machine: String,
    /// The accounts, by label.
    pub accounts: BTreeMap<String, Account>,
    /// Machine-local: the one account this home runs on.
    pub active: Option<String>,
    /// Machine-local: the account `active` was before the last rotation or pin ([`strike`]).
    pub previous: Option<String>,
    /// Machine-local: whether `previous` had served a response when it was left.
    pub previous_proven: bool,
    /// Machine-local: `active` has served a response since it was chosen.
    pub proven: bool,
    /// Machine-local: the session on an `active` that is not proven yet.
    pub canary: Option<Canary>,
    /// Machine-local: what each rotation that can still be gone back from wrote over, newest last
    /// ([`Undo`], [`MAX_UNDO`]).
    pub undo: Vec<Undo>,
    /// Machine-local: `active` is the owner's word.
    pub pinned: bool,
    /// Machine-local: only this explicit selection may pin after signing in.
    pub pending_pin: Option<PinRequest>,
    /// Machine-local: the supervised sessions, by session id.
    pub sessions: BTreeMap<String, SessionRow>,
    /// Machine-local: when `active` was rotated, the last hour's.
    pub rotations: Vec<i64>,
    /// Machine-local: the probe's claim.
    pub probe: Option<ProbeClaim>,
    /// Machine-local: the reset's claim.
    pub reset: Option<ResetClaim>,
}

impl Default for State {
    fn default() -> State {
        State {
            schema: SCHEMA,
            machine: String::new(),
            accounts: BTreeMap::new(),
            active: None,
            previous: None,
            previous_proven: false,
            proven: false,
            canary: None,
            undo: Vec::new(),
            pinned: false,
            pending_pin: None,
            sessions: BTreeMap::new(),
            rotations: Vec::new(),
            probe: None,
            reset: None,
        }
    }
}

/// Reason `state.json` or `logins.json` failed deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// It is not JSON: a file cut short.
    Torn(String),
    /// It is JSON and it is not this build's.
    NotOurs(String),
}

impl Unread {
    /// Human-readable description of the parse or validation failure.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unread::Torn(why) | Unread::NotOurs(why) => why,
        }
    }
}

fn recent(t: i64, now: i64, window: i64) -> bool {
    (0..window).contains(&now.saturating_sub(t))
}

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64)
}

const fn stamp_cap(now: i64) -> i64 {
    now.saturating_add(STAMP_SKEW_S)
}

fn stamp(v: Option<&Value>, cap: i64) -> Option<i64> {
    int(v).map(|at| at.min(cap))
}

fn count(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn pct(v: Option<&Value>) -> Option<u32> {
    count(v).filter(|p| *p <= PCT_MAX)
}

fn word(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| one_line(s, max))
        .filter(|s| !s.is_empty())
}

fn label(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| admissible_label(s))
        .map(str::to_owned)
}

fn wall_json(w: &Wall) -> Value {
    let mut o = Map::new();
    o.insert("until".to_owned(), Value::from(w.until));
    o.insert("at".to_owned(), Value::from(w.at));
    o.insert("by".to_owned(), Value::from(w.by.as_str()));
    o.insert("attempts".to_owned(), Value::from(w.attempts));
    o.insert("cleared".to_owned(), Value::from(w.cleared));
    Value::Object(o)
}

fn wall_of(v: &Value, cap: i64) -> Option<Wall> {
    Some(Wall {
        until: int(v.get("until")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        attempts: count(v.get("attempts")).unwrap_or(0),
        cleared: v.get("cleared").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn sample_json(s: &Sample) -> Value {
    let mut o = Map::new();
    o.insert("session_pct".to_owned(), Value::from(s.session_pct));
    o.insert("session_reset".to_owned(), Value::from(s.session_reset));
    o.insert("week_pct".to_owned(), Value::from(s.week_pct));
    o.insert("week_reset".to_owned(), Value::from(s.week_reset));
    if let Some(at) = s.session_read_at {
        o.insert("session_read_at".to_owned(), Value::from(at));
    }
    if let Some(at) = s.week_read_at {
        o.insert("week_read_at".to_owned(), Value::from(at));
    }
    o.insert("at".to_owned(), Value::from(s.at));
    o.insert("by".to_owned(), Value::from(s.by.as_str()));
    Value::Object(o)
}

fn sample_of(v: &Value, cap: i64) -> Option<Sample> {
    let at = stamp(v.get("at"), cap)?;
    Some(Sample {
        session_pct: pct(v.get("session_pct")),
        session_reset: int(v.get("session_reset")),
        week_pct: pct(v.get("week_pct")),
        week_reset: int(v.get("week_reset")),
        session_read_at: stamp(v.get("session_read_at"), cap),
        week_read_at: stamp(v.get("week_read_at"), cap),
        at,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
    })
}

fn offer_json(o: &Offer) -> Value {
    let mut m = Map::new();
    if let Some(kind) = o.kind {
        m.insert("kind".to_owned(), Value::from(kind.word()));
    }
    m.insert("left".to_owned(), Value::from(o.left));
    m.insert("use_by".to_owned(), Value::from(o.use_by));
    m.insert("at".to_owned(), Value::from(o.at));
    m.insert("by".to_owned(), Value::from(o.by.as_str()));
    Value::Object(m)
}

fn offer_of(v: &Value, cap: i64) -> Option<Offer> {
    Some(Offer {
        kind: v
            .get("kind")
            .and_then(Value::as_str)
            .and_then(ResetKind::of_word),
        left: count(v.get("left"))?,
        use_by: int(v.get("use_by")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
    })
}

fn canary_json(c: &Canary) -> Value {
    let mut m = Map::new();
    m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
    m.insert("at".to_owned(), Value::from(c.at));
    m.insert("took".to_owned(), Value::from(c.took));
    Value::Object(m)
}

fn canary_of(v: &Value) -> Option<Canary> {
    Some(Canary {
        sid: word(v.get("sid"), WORD_MAX).unwrap_or_default(),
        at: int(v.get("at"))?,
        took: v.get("took").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn undo_json(u: &Undo) -> Value {
    let mut m = Map::new();
    m.insert("previous".to_owned(), Value::from(u.previous.clone()));
    m.insert("previous_proven".to_owned(), Value::from(u.previous_proven));
    m.insert(
        "canary".to_owned(),
        u.canary.as_ref().map_or(Value::Null, canary_json),
    );
    Value::Object(m)
}

fn undo_of(v: &Value) -> Option<Undo> {
    v.is_object().then(|| Undo {
        previous: label(v.get("previous")),
        previous_proven: v
            .get("previous_proven")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        canary: v.get("canary").and_then(canary_of),
    })
}

fn account_json(a: &Account) -> Value {
    let mut o = Map::new();
    o.insert(
        "walls".to_owned(),
        Value::Object(
            a.walls
                .iter()
                .map(|(k, w)| (k.clone(), wall_json(w)))
                .collect(),
        ),
    );
    let sample = |s: &Option<Sample>| s.as_ref().map_or(Value::Null, sample_json);
    o.insert("sample".to_owned(), sample(&a.sample));
    o.insert("before".to_owned(), sample(&a.before));
    o.insert(
        "offer".to_owned(),
        a.offer.as_ref().map_or(Value::Null, offer_json),
    );
    o.insert("auth".to_owned(), Value::from(a.auth.as_str()));
    o.insert("auth_at".to_owned(), Value::from(a.auth_at));
    o.insert("cleared_at".to_owned(), Value::from(a.cleared_at));
    o.insert("ran_at".to_owned(), Value::from(a.ran_at));
    o.insert(
        "credits".to_owned(),
        a.credits.as_ref().map_or(Value::Null, credits_json),
    );
    o.insert("credits_day".to_owned(), Value::from(a.credits_day));
    o.insert(
        "warned_session".to_owned(),
        a.warned_session.as_ref().map_or(Value::Null, window_json),
    );
    o.insert(
        "warned_week".to_owned(),
        a.warned_week.as_ref().map_or(Value::Null, window_json),
    );
    o.insert("key".to_owned(), Value::from(a.key.map(|k| k.to_string())));
    o.insert("keyed_at".to_owned(), Value::from(a.keyed_at));
    o.insert(
        "windows".to_owned(),
        Value::Object(
            a.windows
                .iter()
                .map(|(k, w)| (k.clone(), fig_json(w)))
                .collect(),
        ),
    );
    Value::Object(o)
}

fn fig_json(w: &WindowFig) -> Value {
    let mut m = Map::new();
    m.insert("pct_x10".to_owned(), Value::from(w.pct_x10));
    m.insert("reset".to_owned(), Value::from(w.reset));
    m.insert("at".to_owned(), Value::from(w.at));
    m.insert("by".to_owned(), Value::from(w.by.as_str()));
    m.insert("source".to_owned(), Value::from(w.source.as_str()));
    m.insert("epoch_at".to_owned(), Value::from(w.epoch_at));
    Value::Object(m)
}

fn fig_of(v: &Value, cap: i64) -> Option<WindowFig> {
    Some(WindowFig {
        pct_x10: count(v.get("pct_x10")).filter(|p| *p <= 10 * PCT_MAX)?,
        reset: int(v.get("reset"))?,
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        source: word(v.get("source"), WORD_MAX).unwrap_or_default(),
        epoch_at: stamp(v.get("epoch_at"), cap),
    })
}

fn credits_json(c: &Credits) -> Value {
    let mut m = Map::new();
    match c {
        Credits::InUse(c) => {
            m.insert("word".to_owned(), Value::from("in-use"));
            m.insert("since".to_owned(), Value::from(c.since));
            m.insert("seen".to_owned(), Value::from(c.seen));
            m.insert("until".to_owned(), Value::from(c.until));
            m.insert("told".to_owned(), Value::from(c.told));
        }
        Credits::Off(off) => {
            m.insert("word".to_owned(), Value::from("off"));
            m.insert("reason".to_owned(), Value::from(off.reason.as_str()));
            m.insert("at".to_owned(), Value::from(off.at));
            m.insert("until".to_owned(), Value::from(off.until));
        }
        Credits::On(at) => {
            m.insert("word".to_owned(), Value::from("on"));
            m.insert("at".to_owned(), Value::from(*at));
        }
    }
    Value::Object(m)
}

fn credits_of(v: &Value, cap: i64) -> Option<Credits> {
    match v.get("word").and_then(Value::as_str)? {
        "in-use" => Some(Credits::InUse(CreditsUse {
            since: stamp(v.get("since"), cap)?,
            seen: stamp(v.get("seen"), cap)?,
            until: int(v.get("until")),
            told: v.get("told").and_then(Value::as_bool).unwrap_or(false),
        })),
        "off" => Some(Credits::Off(CreditsOff {
            reason: word(v.get("reason"), WORD_MAX).unwrap_or_default(),
            at: stamp(v.get("at"), cap)?,
            until: int(v.get("until")),
        })),
        "on" => Some(Credits::On(stamp(v.get("at"), cap)?)),
        _ => None,
    }
}

fn window_json(w: &WindowSaid) -> Value {
    let mut m = Map::new();
    m.insert("pct".to_owned(), Value::from(w.pct));
    m.insert("reset".to_owned(), Value::from(w.reset));
    m.insert("at".to_owned(), Value::from(w.at));
    Value::Object(m)
}

fn window_of(v: &Value, cap: i64) -> Option<WindowSaid> {
    Some(WindowSaid {
        pct: pct(v.get("pct")),
        reset: int(v.get("reset")),
        at: stamp(v.get("at"), cap)?,
    })
}

fn account_of(v: &Value, now: i64) -> Account {
    let cap = stamp_cap(now);
    let mut walls: Vec<(String, Wall)> = v
        .get("walls")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, w)| {
            Some((
                Some(one_line(k, WORD_MAX)).filter(|k| !k.is_empty())?,
                wall_of(w, cap)?,
            ))
        })
        .collect();
    walls.sort_by_key(|(_, w)| std::cmp::Reverse(w.at));
    walls.truncate(MAX_WALLS);
    let mut windows: Vec<(String, WindowFig)> = v
        .get("windows")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, w)| {
            Some((
                Some(one_line(k, WORD_MAX)).filter(|k| !k.is_empty())?,
                fig_of(w, cap)?,
            ))
        })
        .collect();
    windows.sort_by_key(|(_, w)| std::cmp::Reverse(w.at));
    windows.truncate(MAX_WINDOWS);
    Account {
        key: v
            .get("key")
            .and_then(Value::as_str)
            .and_then(super::profile::AccountKey::parse),
        keyed_at: stamp(v.get("keyed_at"), now),
        windows: windows.into_iter().collect(),
        walls: walls.into_iter().collect(),
        sample: v.get("sample").and_then(|s| sample_of(s, cap)),
        before: v.get("before").and_then(|s| sample_of(s, cap)),
        offer: v.get("offer").and_then(|o| offer_of(o, cap)),
        auth: v
            .get("auth")
            .and_then(Value::as_str)
            .map_or(Auth::Unseen, Auth::parse),
        auth_at: stamp(v.get("auth_at"), now),
        cleared_at: stamp(v.get("cleared_at"), now),
        ran_at: stamp(v.get("ran_at"), now),
        credits: v.get("credits").and_then(|c| credits_of(c, cap)),
        credits_day: int(v.get("credits_day")),
        warned_session: v.get("warned_session").and_then(|w| window_of(w, cap)),
        warned_week: v.get("warned_week").and_then(|w| window_of(w, cap)),
    }
}

impl State {
    /// Serializes the state as a single-line sorted JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(self.schema));
        o.insert("machine".to_owned(), Value::from(self.machine.as_str()));
        o.insert(
            "accounts".to_owned(),
            Value::Object(
                self.accounts
                    .iter()
                    .map(|(k, a)| (k.clone(), account_json(a)))
                    .collect(),
            ),
        );
        o.insert("active".to_owned(), Value::from(self.active.clone()));
        o.insert("previous".to_owned(), Value::from(self.previous.clone()));
        o.insert(
            "previous_proven".to_owned(),
            Value::from(self.previous_proven),
        );
        o.insert("proven".to_owned(), Value::from(self.proven));
        o.insert(
            "canary".to_owned(),
            self.canary.as_ref().map_or(Value::Null, canary_json),
        );
        o.insert(
            "undo".to_owned(),
            Value::Array(self.undo.iter().map(undo_json).collect()),
        );
        o.insert("pinned".to_owned(), Value::from(self.pinned));
        if let Some(request) = &self.pending_pin {
            let mut pending = Map::new();
            pending.insert("label".to_owned(), Value::from(request.label.as_str()));
            pending.insert("nonce".to_owned(), Value::from(request.nonce.as_str()));
            o.insert("pending_pin".to_owned(), Value::Object(pending));
        }
        o.insert(
            "sessions".to_owned(),
            Value::Object(
                self.sessions
                    .iter()
                    .map(|(sid, s)| {
                        let mut row = Map::new();
                        row.insert("account".to_owned(), Value::from(s.account.as_str()));
                        row.insert("at".to_owned(), Value::from(s.at));
                        row.insert("held".to_owned(), Value::from(s.held.clone()));
                        (sid.clone(), Value::Object(row))
                    })
                    .collect(),
            ),
        );
        o.insert(
            "rotations".to_owned(),
            Value::Array(self.rotations.iter().map(|t| Value::from(*t)).collect()),
        );
        o.insert(
            "probe".to_owned(),
            self.probe.as_ref().map_or(Value::Null, |c| {
                let mut m = Map::new();
                m.insert("account".to_owned(), Value::from(c.account.as_str()));
                m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
                m.insert("at".to_owned(), Value::from(c.at));
                Value::Object(m)
            }),
        );
        o.insert(
            "reset".to_owned(),
            self.reset.as_ref().map_or(Value::Null, |c| {
                let mut m = Map::new();
                m.insert("account".to_owned(), Value::from(c.account.as_str()));
                m.insert("until".to_owned(), Value::from(c.until));
                m.insert("window".to_owned(), Value::from(c.window.clone()));
                m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
                m.insert("at".to_owned(), Value::from(c.at));
                m.insert("pid".to_owned(), Value::from(c.pid));
                m.insert("start".to_owned(), Value::from(c.start.as_str()));
                m.insert("done".to_owned(), Value::from(c.done.clone()));
                Value::Object(m)
            }),
        );
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Deserializes and sanitizes `state.json` against `now`.
    /// # Errors
    ///
    /// [`Unread::Torn`]: not JSON (a file cut short, which the next write may replace).
    /// [`Unread::NotOurs`]: JSON but not an object, or naming no schema or one other than
    /// [`SCHEMA`]: a file this build must not guess at or write over.
    pub fn from_json(text: &str, now: i64) -> Result<State, Unread> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|e| Unread::Torn(format!("not JSON ({})", shown(&e.to_string()))))?;
        if !doc.is_object() {
            return Err(Unread::NotOurs("not a JSON object".to_string()));
        }
        match doc.get("schema").and_then(Value::as_u64) {
            Some(SCHEMA) => {}
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schema {SCHEMA}"
                )));
            }
            None => return Err(Unread::NotOurs("names no schema".to_string())),
        }
        let accounts: BTreeMap<String, Account> = doc
            .get("accounts")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, v)| admissible_label(k) && v.is_object())
            .take(MAX_ACCOUNTS)
            .map(|(k, v)| (k.clone(), account_of(v, now)))
            .collect();
        let mut sessions: Vec<(String, SessionRow)> = doc
            .get("sessions")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(sid, v)| {
                Some((
                    Some(one_line(sid, WORD_MAX)).filter(|sid| !sid.is_empty())?,
                    SessionRow {
                        account: label(v.get("account"))?,
                        at: int(v.get("at"))?,
                        held: word(v.get("held"), WORD_MAX),
                    },
                ))
            })
            .collect();
        sessions.sort_by(|a, b| b.1.at.cmp(&a.1.at).then_with(|| a.0.cmp(&b.0)));
        sessions.truncate(MAX_SESSIONS);
        let mut rotations: Vec<i64> = doc
            .get("rotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        rotations.sort_unstable();
        let extra = rotations.len().saturating_sub(MAX_ROTATIONS);
        rotations.drain(..extra);
        let probe = doc.get("probe").and_then(|c| {
            Some(ProbeClaim {
                account: label(c.get("account"))?,
                sid: word(c.get("sid"), WORD_MAX)?,
                at: int(c.get("at"))?,
            })
        });
        let reset = doc.get("reset").and_then(|c| {
            Some(ResetClaim {
                account: label(c.get("account"))?,
                until: int(c.get("until")),
                window: word(c.get("window"), WORD_MAX),
                sid: word(c.get("sid"), WORD_MAX)?,
                at: int(c.get("at"))?,
                pid: count(c.get("pid")).unwrap_or(0),
                start: word(c.get("start"), WORD_MAX).unwrap_or_default(),
                done: word(c.get("done"), WORD_MAX),
            })
        });
        let canary = doc.get("canary").and_then(canary_of);
        let made = canary.as_ref().is_some_and(|c| !c.took);
        let mut undo: Vec<Undo> = doc
            .get("undo")
            .and_then(Value::as_array)
            .filter(|_| made)
            .into_iter()
            .flatten()
            .filter_map(undo_of)
            .collect();
        let extra = undo.len().saturating_sub(MAX_UNDO);
        undo.drain(..extra);
        Ok(State {
            schema: SCHEMA,
            machine: doc
                .get("machine")
                .and_then(Value::as_str)
                .filter(|m| {
                    !m.is_empty() && m.len() <= WORD_MAX && aterm_codec::hex::is_hex(m, m.len())
                })
                .map(str::to_owned)
                .unwrap_or_default(),
            accounts,
            active: label(doc.get("active")),
            previous: label(doc.get("previous")),
            previous_proven: doc
                .get("previous_proven")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            proven: doc.get("proven").and_then(Value::as_bool).unwrap_or(false),
            canary,
            undo,
            pinned: doc.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            pending_pin: doc.get("pending_pin").and_then(|pending| {
                PinRequest::new(
                    pending.get("label")?.as_str()?,
                    pending.get("nonce")?.as_str()?.to_owned(),
                )
            }),
            sessions: sessions.into_iter().collect(),
            rotations,
            probe,
            reset,
        })
    }

    /// Returns the [`Account`] record for `label`, or a default empty record if unseen.
    #[must_use]
    pub fn account(&self, label: &str) -> Account {
        self.accounts.get(label).cloned().unwrap_or_default()
    }

    /// Returns the timestamp when the active canary hold began.
    #[must_use]
    pub fn canary_since(&self) -> Option<i64> {
        (self.active.is_some() && !self.proven)
            .then(|| self.canary.as_ref().map(|c| c.at))
            .flatten()
    }

    /// Returns whether an unproven canary hold is currently active at `now`.
    #[must_use]
    pub fn canary_holds(&self, now: i64) -> bool {
        self.canary_since()
            .is_some_and(|at| recent(at, now, CANARY_HOLD_S))
    }

    /// Returns the timestamp when the active canary hold expires at `now`.
    #[must_use]
    pub fn canary_until(&self, now: i64) -> Option<i64> {
        self.canary_since()
            .filter(|_| self.canary_holds(now))
            .map(|since| since.saturating_add(CANARY_HOLD_S))
    }

    /// Returns the session IDs currently recorded on `label` in sorted order.
    #[must_use]
    pub fn sessions_on(&self, label: &str) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.account == label)
            .map(|(sid, _)| sid.as_str())
            .collect()
    }
}

/// Generates a fresh 16-hex-char machine identifier from OS entropy.
/// # Errors
///
/// No OS entropy source answered.
pub fn mint_machine() -> io::Result<String> {
    aterm_uds::rand::hex_token::<8>()
}

/// Derived operational condition of a roster account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// No login was made for it on this machine ([`login_made`]).
    NotEnrolled,
    /// Its login's last word on record is [`Auth::SignedOut`] ([`strike`]).
    SignedOut,
    /// A wall stands, until this instant.
    Limited {
        /// When its last standing wall ends.
        until: i64,
    },
    /// A window still running is at its threshold, however near its reset.
    Low,
    /// Observed, and none of the above.
    Usable,
    /// Never observed.
    Unseen,
}

impl Condition {
    /// Canonical condition word for reports and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Condition::NotEnrolled => "not-enrolled",
            Condition::SignedOut => "signed-out",
            Condition::Limited { .. } => "limited",
            Condition::Low => "low",
            Condition::Usable => "usable",
            Condition::Unseen => "unseen",
        }
    }

    /// Returns whether the account has fresh budget (`Usable` or `Unseen`).
    #[must_use]
    pub const fn fresh(self) -> bool {
        matches!(self, Condition::Usable | Condition::Unseen)
    }

    /// Returns whether a session can run on the account now (`Usable`, `Unseen` or `Low`).
    #[must_use]
    pub fn runs(self) -> bool {
        self.fresh() || self == Condition::Low
    }
}

/// Computes the [`Condition`] of `row` at `now` given `state` and whether its login is `enrolled`.
#[must_use]
pub fn condition(row: &Row, state: &State, enrolled: bool, now: i64) -> Condition {
    if !enrolled {
        return Condition::NotEnrolled;
    }
    let Some(account) = state.accounts.get(&row.label) else {
        return Condition::Unseen;
    };
    if account.auth == Auth::SignedOut {
        return Condition::SignedOut;
    }
    if let Some(until) = account.limited_until(now) {
        return Condition::Limited { until };
    }
    if account.low(now) {
        return Condition::Low;
    }
    if account.observed() {
        Condition::Usable
    } else {
        Condition::Unseen
    }
}

/// Returns the labels of accounts in [`Condition::SignedOut`] in roster order.
#[must_use]
pub fn signed_out(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            row.email.is_none()
                && condition(row, state, is_enrolled(row, enrolled), now) == Condition::SignedOut
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Returns the first signed-out roster row that would give a session on `me` a usable target if
/// signed in.
#[must_use]
pub fn sign_in_next(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    now: i64,
) -> Option<(String, bool)> {
    roster
        .rows
        .iter()
        .filter(|row| me != Some(row.label.as_str()))
        .find(|row| {
            matches!(
                condition(row, state, is_enrolled(row, enrolled), now),
                Condition::NotEnrolled | Condition::SignedOut
            ) && !state
                .accounts
                .get(&row.label)
                .is_some_and(|a| a.limited_until(now).is_some())
        })
        .map(|row| (row.label.clone(), row.email.is_some()))
}

/// Returns the first signed-out or un-enrolled roster row with an email (`Dedicated` before
/// `Shared`) that would give a session on `me` a usable target if renewed.
#[must_use]
pub fn renew_next(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    now: i64,
) -> Option<String> {
    for class in [Class::Dedicated, Class::Shared] {
        for row in roster.rows.iter().filter(|r| r.class == class) {
            if me == Some(row.label.as_str()) || row.email.is_none() {
                continue;
            }
            if matches!(
                condition(row, state, is_enrolled(row, enrolled), now),
                Condition::NotEnrolled | Condition::SignedOut
            ) && !state
                .accounts
                .get(&row.label)
                .is_some_and(|a| a.limited_until(now).is_some())
            {
                return Some(row.label.clone());
            }
        }
    }
    None
}

/// `[harness] accounts` rotation policy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Move just before the wall, and at it.
    Early,
    /// Move only at the wall.
    Wall,
    /// Nothing rotates.
    Off,
}

impl Mode {
    /// Canonical `aterm.toml` string for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Early => "early",
            Mode::Wall => "wall",
            Mode::Off => "off",
        }
    }

    /// Parses a [`Mode`] from its `aterm.toml` string.
    #[must_use]
    pub fn parse(word: &str) -> Option<Mode> {
        match word {
            "early" => Some(Mode::Early),
            "wall" => Some(Mode::Wall),
            "off" => Some(Mode::Off),
            _ => None,
        }
    }
}

/// Trigger point at which a session asks [`advise`] for account guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Point {
    /// The turn ended on a usage limit this process hit ([`note_wall`], [`begin_rotation`]).
    Wall {
        /// The vendor's limit type (`five_hour`, `seven_day`, …).
        window: String,
        /// When the limit's row was written ([`Advice::Resume`]).
        at: Option<i64>,
    },
    /// A lost login right after the session's own move ([`advise`], [`strike`]).
    LostLogin {
        /// When the host first met this process's lost login (Unix seconds, its own clock).
        seen: i64,
    },
    /// A lost login with no move behind it ([`note_login_end`]).
    LoginEnded,
    /// A turn that ended with none.
    TurnEnd,
}

/// Session identity and login facts passed to [`advise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Me<'a> {
    /// The label its process's own environment names ([`Roster::row_for_login`]).
    pub account: Option<&'a str>,
    /// The label whose login a renewal on this machine moved away from the one its process holds
    /// ([`Roster::row_left`], [`bound`]).
    pub left: Option<&'a str>,
    /// Its Claude home is the default one.
    pub default_home: bool,
    /// Its process authenticates by a stored login.
    pub stored_login: bool,
}

impl<'a> Me<'a> {
    /// Returns the roster label for the session's current or superseded login.
    #[must_use]
    pub fn label(&self) -> Option<&'a str> {
        self.account.or(self.left)
    }
}

/// Reason [`advise`] recommends staying on the current account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StayWhy {
    /// `accounts = "off"`.
    Off,
    /// No roster.
    NoRoster,
    /// The session's login is no row of the roster.
    NotListed,
    /// The session's home is not the default.
    OtherHome,
    /// The process does not authenticate by a stored login.
    NotAStoredLogin,
    /// `active` has not served a response yet: nobody follows it, and nobody rotates past it.
    Canary,
    /// At a wall with no candidate and no instant of the roster's to wait for: the wall is waited
    /// out as it would be with no roster.
    NoCandidate,
    /// The machine's rotation bound is spent.
    Breaker {
        /// When the bound has room again ([`breaker_until`]).
        until: i64,
    },
    /// An account to leave ahead of its wall, and the owner's pin holds.
    Pinned,
    /// An account to leave, and it holds a reset offer that expires within a day.
    OfferExpiring,
    /// An account to leave, and no fresh account to move to.
    NoFresh,
    /// Nothing is due.
    NothingDue,
    /// A move is due, and no lab pass has measured this CLI's seam ([`advise_for`]), or its walls
    /// for a running session ([`advise_session`]).
    Unmeasured,
    /// The session answers its own walls (a proven self-claim, `codex-alab@<pid>`): aterm moves
    /// nothing under it ([`advise_session`]).
    OtherSupervisor,
    /// The CLI keeps its login in the OS keyring here (Codex's `cli_auth_credentials_store`),
    /// where no row's own login file is read, so no row is picked or moved to.
    KeyringStore,
    /// A move is due, and the relaunch line onto the row could not be written (its overlay home
    /// could not be made, or holds entries of its own): nothing was committed.
    NoLine,
}

impl StayWhy {
    /// Short reason word for logs and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StayWhy::Off => "off",
            StayWhy::NoRoster => "no-roster",
            StayWhy::NotListed => "not-listed",
            StayWhy::OtherHome => "other-home",
            StayWhy::NotAStoredLogin => "not-a-stored-login",
            StayWhy::Canary => "canary",
            StayWhy::NoCandidate => "no-candidate",
            StayWhy::Breaker { .. } => "breaker",
            StayWhy::Pinned => "pinned",
            StayWhy::OfferExpiring => "offer-expiring",
            StayWhy::NoFresh => "no-fresh",
            StayWhy::NothingDue => "nothing-due",
            StayWhy::Unmeasured => "unmeasured",
            StayWhy::OtherSupervisor => "other-supervisor",
            StayWhy::KeyringStore => "keyring-store",
            StayWhy::NoLine => "no-line",
        }
    }
}

/// Action recommended by [`advise`] at a session turn boundary or wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advice {
    /// Do what would be done with no roster.
    Stay(StayWhy),
    /// Read `/usage` (after [`claim_probe`]).
    Probe,
    /// Relaunch under another login.
    Move {
        /// The label to move to.
        to: String,
        /// Ahead of the wall.
        ahead: bool,
        /// This move changes `active` and needs [`begin_rotation`] to win.
        rotates: bool,
        /// `active` has not served a response and the session that moved onto it has been silent
        /// for [`CANARY_HOLD_S`] ([`take_canary`]).
        takes_canary: bool,
    },
    /// No account is usable before this instant.
    Wait {
        /// When to decide again.
        until: i64,
    },
    /// The owner lifted this wall ([`pin`]).
    Resume {
        /// When the owner cleared the account ([`Account::cleared_at`], as the state was read).
        cleared_at: i64,
    },
}

impl Advice {
    /// Short action word logged by the host.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Advice::Stay(_) => "stay",
            Advice::Probe => "probe",
            Advice::Move { .. } => "move",
            Advice::Wait { .. } => "wait",
            Advice::Resume { .. } => "resume",
        }
    }
}

/// Sort key ordering candidate target accounts (lower ranks first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rank {
    /// `low`: admitted only at a wall.
    pub low: bool,
    /// The row's class.
    pub class: Class,
    /// The week's reset from the newest sample, while it is still ahead.
    pub week_reset: Option<i64>,
}

impl Rank {
    fn key<'a>(&self, label: &'a str) -> (bool, Class, bool, i64, &'a str) {
        (
            self.low,
            self.class,
            self.week_reset.is_none(),
            self.week_reset.unwrap_or(i64::MAX),
            label,
        )
    }
}

/// Reason a roster row is excluded from candidate rotation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNot {
    /// It is the session's own account.
    Me,
    /// No login was made for it on this machine ([`login_made`]).
    NotEnrolled,
    /// Its login is known signed out.
    SignedOut,
    /// A wall stands.
    Limited {
        /// Until this instant.
        until: i64,
    },
    /// `low`, and the point is not a wall.
    LowAheadOfAWall,
    /// `low`, and an account becomes usable within [`LOW_ONLY_AFTER_S`].
    LowWhileOneIsNear {
        /// When that account is usable.
        at: i64,
    },
}

impl WhyNot {
    /// Human-readable exclusion reason for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WhyNot::Me => "this session's own",
            WhyNot::NotEnrolled => "not-enrolled",
            WhyNot::SignedOut => "signed-out",
            WhyNot::Limited { .. } => "limited",
            WhyNot::LowAheadOfAWall => "low: used only at a wall",
            WhyNot::LowWhileOneIsNear { .. } => "low: an account is usable within 15 minutes",
        }
    }
}

fn usable_at(row: &Row, state: &State, cond: Condition, now: i64) -> Option<i64> {
    match cond {
        Condition::Limited { until } => Some(until),
        Condition::Usable | Condition::Unseen => Some(now),
        Condition::Low => state
            .accounts
            .get(&row.label)
            .and_then(|a| a.low_until(now)),
        Condition::SignedOut | Condition::NotEnrolled => None,
    }
}

/// Ranks all roster rows as rotation candidates.
#[must_use]
pub fn candidates(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    point: &Point,
    now: i64,
) -> Vec<(String, Result<Rank, WhyNot>)> {
    let conditions: Vec<(&Row, Condition)> = roster
        .rows
        .iter()
        .map(|r| (r, condition(r, state, is_enrolled(r, enrolled), now)))
        .collect();
    let near = conditions
        .iter()
        .filter_map(|(row, c)| usable_at(row, state, *c, now))
        .filter(|at| *at > now && at.saturating_sub(now) <= LOW_ONLY_AFTER_S)
        .min();
    let at_wall = matches!(
        point,
        Point::Wall { .. } | Point::LostLogin { .. } | Point::LoginEnded
    );
    let mut out: Vec<(String, Result<Rank, WhyNot>)> = conditions
        .iter()
        .map(|(row, cond)| {
            let rank = |low: bool| Rank {
                low,
                class: row.class,
                week_reset: state
                    .accounts
                    .get(&row.label)
                    .and_then(|a| a.week_reset(now)),
            };
            let verdict = if me == Some(row.label.as_str()) {
                Err(WhyNot::Me)
            } else {
                match *cond {
                    Condition::NotEnrolled => Err(WhyNot::NotEnrolled),
                    Condition::SignedOut => Err(WhyNot::SignedOut),
                    Condition::Limited { until } => Err(WhyNot::Limited { until }),
                    Condition::Low if !at_wall => Err(WhyNot::LowAheadOfAWall),
                    Condition::Low => match near {
                        Some(at) => Err(WhyNot::LowWhileOneIsNear { at }),
                        None => Ok(rank(true)),
                    },
                    Condition::Usable | Condition::Unseen => Ok(rank(false)),
                }
            };
            (row.label.clone(), verdict)
        })
        .collect();
    out.sort_by(|a, b| match (&a.1, &b.1) {
        (Ok(x), Ok(y)) => x.key(&a.0).cmp(&y.key(&b.0)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => std::cmp::Ordering::Equal,
    });
    out
}

/// Earliest timestamp at which any roster account becomes usable, or the earliest dedicated one's
/// when that is within [`DEDICATED_WAIT_S`] of it.
#[must_use]
pub fn next_usable(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::All, now)
}

/// Earliest timestamp after `now` at which a currently blocked roster account becomes usable.
#[must_use]
pub fn next_opening(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::Ahead, now)
}

/// Timestamp at which a session held at a wall should re-evaluate [`advise`].
#[must_use]
pub fn hold_until(why: StayWhy, state: &State, opening: Option<i64>, now: i64) -> Option<i64> {
    let end = match why {
        StayWhy::Canary => state.canary_until(now)?,
        StayWhy::Breaker { until } => until,
        _ => return None,
    };
    Some(opening.filter(|at| *at > now).map_or(end, |at| end.min(at)))
}

/// Which of the roster's instants [`wait_instant`] counts.
#[derive(Debug, Clone, Copy)]
enum Instants<'a> {
    /// Every account's.
    All,
    /// Every account's, but only one ahead of `now` for this one.
    WalledOn(&'a str),
    /// Only the ones ahead of `now` ([`next_opening`]).
    Ahead,
}

fn wait_instant(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    which: Instants<'_>,
    now: i64,
) -> Option<i64> {
    let instants: Vec<(Class, i64)> = roster
        .rows
        .iter()
        .filter_map(|r| {
            let cond = condition(r, state, is_enrolled(r, enrolled), now);
            let at = usable_at(r, state, cond, now)?;
            let counted = match which {
                Instants::All => true,
                Instants::WalledOn(walled) => walled != r.label || at > now,
                Instants::Ahead => at > now,
            };
            counted.then_some((r.class, at))
        })
        .collect();
    let earliest = instants.iter().map(|(_, t)| *t).min()?;
    let dedicated = instants
        .iter()
        .filter(|(class, _)| *class == Class::Dedicated)
        .map(|(_, t)| *t)
        .min();
    Some(match dedicated {
        Some(t) if t.saturating_sub(earliest) <= DEDICATED_WAIT_S => t,
        _ => earliest,
    })
}

/// Computes the seconds after its newest sample when `account`'s next `/usage` probe becomes due.
#[must_use]
pub fn probe_interval(account: &Account) -> i64 {
    let (Some(new), Some(old)) = (&account.sample, &account.before) else {
        return PROBE_DEFAULT_S;
    };
    let span = new.at.saturating_sub(old.at);
    if span <= 0 {
        return PROBE_DEFAULT_S;
    }
    let half_to = |new_pct: Option<u32>, old_pct: Option<u32>, same: bool, threshold: u32| {
        let (new_pct, old_pct) = (new_pct?, old_pct?);
        if !same {
            return None;
        }
        if new_pct >= threshold {
            return Some(0);
        }
        if new_pct <= old_pct {
            return Some(i64::MAX);
        }
        let left = i128::from(threshold - new_pct);
        let gained = i128::from(new_pct - old_pct);
        let half = left * i128::from(span) / (2 * gained);
        Some(i64::try_from(half).unwrap_or(i64::MAX))
    };
    let known = [
        half_to(
            new.session_pct,
            old.session_pct,
            new.session_reset == old.session_reset,
            LOW_SESSION_PCT,
        ),
        half_to(
            new.week_pct,
            old.week_pct,
            new.week_reset == old.week_reset,
            LOW_WEEK_PCT,
        ),
    ];
    known
        .into_iter()
        .flatten()
        .min()
        .map_or(PROBE_DEFAULT_S, |s| s.clamp(PROBE_MIN_S, PROBE_MAX_S))
}

/// Returns whether a `/usage` probe is due for `account` at `now`.
#[must_use]
pub fn probe_due(account: &Account, now: i64) -> bool {
    account
        .sample
        .as_ref()
        .is_none_or(|s| !recent(s.at, now, probe_interval(account)))
}

fn offer_expiring(account: &Account, now: i64) -> bool {
    account.offer.as_ref().is_some_and(|o| {
        o.left > 0
            && o.use_by
                .is_some_and(|by| by > now && by.saturating_sub(now) <= OFFER_EXPIRING_S)
    })
}

/// Pure decision function returning the [`Advice`] for session `me` at `point` and `now`.
#[must_use]
pub fn advise(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    advise_with(roster, state, enrolled, me, point, mode, now, true)
}

/// Computes the advice for `cli`'s pool: [`advise`]'s for Claude Code, else [`advise_past_probe`]'s
/// (another CLI has no `/usage` probe). A move between the logins of a CLI whose seam no lab pass
/// has measured ([`crate::ai::connector::Accounts::measured`]) becomes [`StayWhy::Unmeasured`],
/// since a relaunch onto an unmeasured seam can lose the session it moves.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn advise_for(
    cli: Cli,
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    let advice = if cli == Cli::Claude {
        advise(roster, state, enrolled, me, point, mode, now)
    } else {
        advise_past_probe(roster, state, enrolled, me, point, mode, now)
    };
    held_unless_measured(advice, cli.accounts().measured)
}

/// `advice`, a move in it held as [`StayWhy::Unmeasured`] where the seam is not `measured`.
fn held_unless_measured(advice: Advice, measured: bool) -> Advice {
    match advice {
        Advice::Move { .. } if !measured => Advice::Stay(StayWhy::Unmeasured),
        advice => advice,
    }
}

/// Who answers a running session's walls ([`advise_session`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisor {
    /// aterm's harness: the roster's advice is its to act on.
    Aterm,
    /// The session's own program, by a proven self-claim (`codex-alab@<pid>`).
    Own,
}

/// Computes [`advise_for`] for a running session of `cli` that `supervisor` answers. A session
/// that answers its own walls stays ([`StayWhy::OtherSupervisor`]), and so does a move of a
/// session of a CLI whose walls no lab pass has measured
/// ([`crate::ai::connector::Accounts::walls_measured_on`], [`StayWhy::Unmeasured`]): that CLI
/// moves only at a launch ([`launch_row`]).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn advise_session(
    cli: Cli,
    supervisor: Supervisor,
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    if supervisor == Supervisor::Own {
        return Advice::Stay(StayWhy::OtherSupervisor);
    }
    match advise_for(cli, roster, state, enrolled, me, point, mode, now) {
        Advice::Move { .. } if cli.accounts().walls_measured_on.is_none() => {
            Advice::Stay(StayWhy::Unmeasured)
        }
        advice => advice,
    }
}

/// The row a launch aterm makes of a pool's CLI starts under (a worker's spawn, a cold restore):
/// `active` while the owner's pin holds it (a row whose login is here and not signed out) or
/// while it is fresh, else the first candidate a wall would move to (`next:` in
/// `aterm accounts`), else `active` while it still runs (a low row, every other one walled);
/// `None` when no row is usable.
#[must_use]
pub fn launch_row<'a>(
    roster: &'a Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<&'a Row> {
    let active = state.active.as_deref().and_then(|label| roster.row(label));
    if let Some(row) = active {
        let cond = condition(row, state, is_enrolled(row, enrolled), now);
        let here = !matches!(cond, Condition::NotEnrolled | Condition::SignedOut);
        if (state.pinned && here) || cond.fresh() {
            return Some(row);
        }
    }
    let point = Point::Wall {
        window: String::new(),
        at: None,
    };
    let me = active.map(|row| row.label.as_str());
    candidates(roster, state, enrolled, me, &point, now)
        .into_iter()
        .find(|(_, verdict)| verdict.is_ok())
        .and_then(|(label, _)| roster.row(&label))
        .or_else(|| {
            active.filter(|row| condition(row, state, is_enrolled(row, enrolled), now).runs())
        })
}

/// Computes [`advise`] with the `/usage` probe arm skipped.
#[must_use]
pub fn advise_past_probe(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    advise_with(roster, state, enrolled, me, point, mode, now, false)
}

/// [`advise`] (`probes`) and [`advise_past_probe`] (not): one body.
#[allow(clippy::too_many_arguments)]
fn advise_with(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
    probes: bool,
) -> Advice {
    let login_point =
        *point == Point::LoginEnded || (me.left.is_some() && !matches!(point, Point::Wall { .. }));
    if mode == Mode::Off && !login_point {
        return Advice::Stay(StayWhy::Off);
    }
    if roster.rows.is_empty() {
        return Advice::Stay(StayWhy::NoRoster);
    }
    let Some(mine) = me.label().and_then(|label| roster.row(label)) else {
        return Advice::Stay(StayWhy::NotListed);
    };
    if !me.default_home {
        return Advice::Stay(StayWhy::OtherHome);
    }
    if !me.stored_login {
        return Advice::Stay(StayWhy::NotAStoredLogin);
    }
    let at_wall = matches!(
        point,
        Point::Wall { .. } | Point::LostLogin { .. } | Point::LoginEnded
    );
    let cond = |row: &Row| condition(row, state, is_enrolled(row, enrolled), now);
    if login_point {
        return login_follow(mine, me, point, cond(mine));
    }
    let onto_active = |active: &Row, takes_canary: bool| Advice::Move {
        to: active.label.clone(),
        ahead: !at_wall,
        rotates: false,
        takes_canary,
    };

    let mut low_active = None;
    if let Some(active) = state.active.as_deref().and_then(|l| roster.row(l))
        && active.label != mine.label
    {
        let theirs = cond(active);
        if theirs.runs() && !state.proven {
            let take = !state.canary_holds(now) && (at_wall || theirs.fresh());
            return if take {
                onto_active(active, true)
            } else {
                Advice::Stay(StayWhy::Canary)
            };
        }
        if theirs.fresh() || (theirs.runs() && state.pinned) {
            return onto_active(active, false);
        }
        if theirs.runs() {
            low_active = Some(active);
        }
    }

    if let Point::Wall {
        window,
        at: Some(written),
    } = point
        && let Some(record) = state.accounts.get(&mine.label)
        && let Some(cleared_at) = record.cleared_since(window, *written, now)
        && cond(mine).runs()
    {
        return Advice::Resume {
            cleared_at: cleared_at.min(now),
        };
    }

    if let Point::LostLogin { seen } = *point
        && cond(mine).runs()
        && state.accounts.get(&mine.label).is_some_and(|a| {
            a.auth == Auth::Ok && a.auth_at.is_some_and(|signed_in| signed_in > seen)
        })
    {
        let unanswered = !state.proven && state.active.as_deref() == Some(mine.label.as_str());
        if unanswered && state.canary_holds(now) {
            return Advice::Stay(StayWhy::Canary);
        }
        return Advice::Move {
            to: mine.label.clone(),
            ahead: false,
            rotates: false,
            takes_canary: unanswered,
        };
    }

    let breaker = breaker_until(state, roster, now);
    let listed = candidates(roster, state, enrolled, Some(&mine.label), point, now);
    let low_active =
        low_active.filter(|a| listed.iter().any(|(l, rank)| *l == a.label && rank.is_ok()));
    let next = listed
        .into_iter()
        .find_map(|(label, verdict)| verdict.ok().map(|rank| (label, rank)));

    if at_wall {
        return match (next, low_active, breaker) {
            (Some((_, rank)), Some(active), spent) if rank.low || spent.is_some() => {
                onto_active(active, false)
            }
            (Some(_), _, Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
            (Some((to, _)), _, None) => Advice::Move {
                to,
                ahead: false,
                rotates: true,
                takes_canary: false,
            },
            (None, ..) => {
                let walled = Instants::WalledOn(&mine.label);
                match wait_instant(roster, state, enrolled, walled, now) {
                    Some(until) if until > now => Advice::Wait { until },
                    _ => Advice::Stay(StayWhy::NoCandidate),
                }
            }
        };
    }

    if mode != Mode::Early {
        return Advice::Stay(StayWhy::NothingDue);
    }
    let account = state.account(&mine.label);
    let own = cond(mine);
    let limited = matches!(own, Condition::Limited { .. });
    let leave = limited || (own == Condition::Low && account.leave_early(now));
    if state.pinned {
        return Advice::Stay(if leave {
            StayWhy::Pinned
        } else {
            StayWhy::NothingDue
        });
    }
    let unanswered =
        !state.proven && state.active.as_deref() == Some(mine.label.as_str()) && own.runs();
    if probes
        && roster.rows.len() > 1
        && !limited
        && !unanswered
        && !probe_held(state, &mine.label, now)
        && probe_due(&account, now)
    {
        return Advice::Probe;
    }
    if !leave {
        return Advice::Stay(StayWhy::NothingDue);
    }
    if offer_expiring(&account, now) {
        return Advice::Stay(StayWhy::OfferExpiring);
    }
    match (next, breaker) {
        (None, _) => Advice::Stay(StayWhy::NoFresh),
        (Some(_), _) if unanswered => Advice::Stay(StayWhy::Canary),
        (Some(_), Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
        (Some((to, _)), None) => Advice::Move {
            to,
            ahead: true,
            rotates: true,
            takes_canary: false,
        },
    }
}

fn login_follow(mine: &Row, me: &Me<'_>, point: &Point, cond: Condition) -> Advice {
    let at_wall = !matches!(point, Point::TurnEnd);
    let signed_in = !matches!(cond, Condition::SignedOut | Condition::NotEnrolled);
    if me.left.is_some() && signed_in {
        return Advice::Move {
            to: mine.label.clone(),
            ahead: !at_wall,
            rotates: false,
            takes_canary: false,
        };
    }
    Advice::Stay(if at_wall {
        StayWhy::NoCandidate
    } else {
        StayWhy::NothingDue
    })
}

/// Event kind recorded in `ledger.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A wall was recorded for an account.
    Wall,
    /// A `/usage` sample was recorded.
    Sample,
    /// `active` was rotated ([`begin_rotation`]), or a canary took a silent one's place
    /// ([`take_canary`]).
    Rotate,
    /// The rotated-to account served a response.
    Proven,
    /// A candidate was struck and `active` went back.
    Struck,
    /// A session was read on another account than before.
    Move,
    /// A `/usage` probe was claimed.
    Probe,
    /// A limit reset was used.
    ResetUsed,
    /// A limit reset was offered and kept.
    ResetKept,
    /// A limit reset failed.
    ResetFailed,
    /// A login's word changed.
    Auth,
    /// The owner pinned an account.
    Use,
    /// The pin ended.
    Unpin,
    /// A tapped session's usage tap went silent (`super::tap::Silence`).
    NoTap,
    /// Usage credits began carrying an account's sessions ([`record_usage`]).
    Credits,
    /// Claude Code warned that a window is filling ([`record_usage`]).
    Warned,
    /// A login was renewed ([`note_renewed`]).
    Renewed,
    /// A bound login was seen ended, at a login wall in a session on it ([`note_login_end`]).
    LoginEnded,
    /// A renewal's check answered another account than the row's ([`note_wrong_account`]).
    WrongAccount,
    /// A limit reset was used inside a window ([`note_reading`], [`WindowRead::refilled`]).
    Reset,
    /// Another account signed in to the home ([`note_reading`]).
    Epoch,
    /// A sign-in that bound no login, `accounts browser`'s window included: its `why` names the
    /// page's host, the flow (`device`, `browser`, `window`) and how it ended, never a URL, code
    /// or email.
    SignIn,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Kind; 22] = [
        Kind::Wall,
        Kind::Sample,
        Kind::Rotate,
        Kind::Proven,
        Kind::Struck,
        Kind::Move,
        Kind::Probe,
        Kind::ResetUsed,
        Kind::ResetKept,
        Kind::ResetFailed,
        Kind::Auth,
        Kind::Use,
        Kind::Unpin,
        Kind::NoTap,
        Kind::Credits,
        Kind::Warned,
        Kind::Renewed,
        Kind::LoginEnded,
        Kind::WrongAccount,
        Kind::Reset,
        Kind::Epoch,
        Kind::SignIn,
    ];

    /// Canonical string for this event kind in `ledger.jsonl`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Wall => "wall",
            Kind::Sample => "sample",
            Kind::Rotate => "rotate",
            Kind::Proven => "proven",
            Kind::Struck => "struck",
            Kind::Move => "move",
            Kind::Probe => "probe",
            Kind::ResetUsed => "reset-used",
            Kind::ResetKept => "reset-kept",
            Kind::ResetFailed => "reset-failed",
            Kind::Auth => "auth",
            Kind::Use => "use",
            Kind::Unpin => "unpin",
            Kind::NoTap => "no-tap",
            Kind::Credits => "credits",
            Kind::Warned => "warned",
            Kind::Renewed => "renewed",
            Kind::LoginEnded => "login-ended",
            Kind::WrongAccount => "wrong-account",
            Kind::Reset => "reset",
            Kind::Epoch => "epoch",
            Kind::SignIn => "sign-in",
        }
    }

    /// Parses a [`Kind`] from its `ledger.jsonl` string.
    #[must_use]
    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == word)
    }
}

/// Single append-only event row in `ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// When (Unix seconds).
    pub t: i64,
    /// What.
    pub kind: Kind,
    /// The account it is about.
    pub account: String,
    /// The account moved or rotated to.
    pub to: Option<String>,
    /// The session, where one is concerned.
    pub sid: Option<String>,
    /// The reason, one bounded line.
    pub why: String,
}

impl Event {
    /// Constructs a ledger event at timestamp `t` for `account`.
    #[must_use]
    pub fn new(t: i64, kind: Kind, account: &str) -> Event {
        Event {
            t,
            kind,
            account: one_line(account, LABEL_MAX),
            to: None,
            sid: None,
            why: String::new(),
        }
    }

    /// Sets the target account label (`to`) on this event.
    #[must_use]
    pub fn with_to(mut self, label: &str) -> Event {
        self.to = Some(one_line(label, LABEL_MAX)).filter(|l| !l.is_empty());
        self
    }

    /// Sets the session ID (`sid`) on this event.
    #[must_use]
    pub fn with_sid(mut self, sid: &str) -> Event {
        self.sid = Some(one_line(sid, WORD_MAX)).filter(|s| !s.is_empty());
        self
    }

    /// Sets the reason string (`why`) on this event.
    #[must_use]
    pub fn because(mut self, why: &str) -> Event {
        self.why = one_line(why, WHY_MAX);
        self
    }

    /// Returns whether this event wakes sessions waiting on an account hold.
    #[must_use]
    pub fn wakes(&self) -> bool {
        match self.kind {
            Kind::Use
            | Kind::Unpin
            | Kind::Proven
            | Kind::Struck
            | Kind::ResetUsed
            | Kind::ResetKept
            | Kind::ResetFailed
            | Kind::Renewed => true,
            Kind::Auth => self.why == Auth::Ok.as_str(),
            Kind::Wall
            | Kind::Sample
            | Kind::Rotate
            | Kind::Move
            | Kind::Probe
            | Kind::NoTap
            | Kind::Credits
            | Kind::Warned
            | Kind::LoginEnded
            | Kind::WrongAccount
            | Kind::Reset
            | Kind::Epoch
            | Kind::SignIn => false,
        }
    }

    /// Serializes the event as a single-line bounded JSON object.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        let mut o = Map::new();
        o.insert("t".to_owned(), Value::from(self.t));
        o.insert("kind".to_owned(), Value::from(self.kind.as_str()));
        o.insert(
            "account".to_owned(),
            Value::from(one_line(&self.account, LABEL_MAX)),
        );
        o.insert(
            "to".to_owned(),
            Value::from(self.to.as_deref().map(|l| one_line(l, LABEL_MAX))),
        );
        o.insert(
            "sid".to_owned(),
            Value::from(self.sid.as_deref().map(|s| one_line(s, WORD_MAX))),
        );
        o.insert("why".to_owned(), Value::from(one_line(&self.why, WHY_MAX)));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Parses a single `ledger.jsonl` line into an [`Event`].
    #[must_use]
    pub fn of_json_line(line: &str) -> Option<Event> {
        if line.len() > MAX_EVENT_LINE_BYTES {
            return None;
        }
        let doc: Value = aterm_json::from_str(line.trim()).ok()?;
        Some(Event {
            t: int(doc.get("t"))?,
            kind: Kind::parse(doc.get("kind")?.as_str()?)?,
            account: word(doc.get("account"), LABEL_MAX)?,
            to: word(doc.get("to"), LABEL_MAX),
            sid: word(doc.get("sid"), WORD_MAX),
            why: word(doc.get("why"), WHY_MAX).unwrap_or_default(),
        })
    }
}

fn account_mut<'a>(state: &'a mut State, label: &str) -> Option<&'a mut Account> {
    if !admissible_label(label) {
        return None;
    }
    if !state.accounts.contains_key(label) && state.accounts.len() >= MAX_ACCOUNTS {
        let stalest = state
            .accounts
            .iter()
            .min_by_key(|(_, a)| a.touched())
            .map(|(k, _)| k.clone())?;
        state.accounts.remove(&stalest);
    }
    Some(state.accounts.entry(label.to_owned()).or_default())
}

fn end_pin(state: &mut State, account: &str, why: &str, t: i64, out: &mut Vec<Event>) {
    if state.pinned && state.active.as_deref() == Some(account) {
        state.pinned = false;
        out.push(Event::new(t, Kind::Unpin, account).because(why));
    }
}

fn most_lapsed(walls: &BTreeMap<String, Wall>) -> Option<String> {
    walls
        .iter()
        .min_by_key(|(_, w)| (w.stands_until(), w.at))
        .map(|(k, _)| k.clone())
}

/// Filters a wall's reported reset timestamp `until`, discarding timestamps more than
/// [`RESET_BELIEVED_S`] past `at`.
#[must_use]
pub fn believed_reset(until: Option<i64>, at: i64) -> Option<i64> {
    until.filter(|until| until.saturating_sub(at) <= RESET_BELIEVED_S)
}

/// Records a quota wall hit by `account` and returns any resulting ledger events.
pub fn note_wall(
    state: &mut State,
    account: &str,
    window: &str,
    until: Option<i64>,
    at: i64,
    now: i64,
) -> Vec<Event> {
    let window = one_line(window, WORD_MAX);
    let until = believed_reset(until, at);
    let by = state.machine.clone();
    let mut out = Vec::new();
    if window.is_empty() {
        return out;
    }
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if record.cleared_since(&window, at, now).is_some() {
        return out;
    }
    let late = u32::from(until.is_none_or(|u| at > u));
    if let Some(old) = record.walls.get_mut(&window) {
        old.at = old.at.min(now);
    }
    let attempts = match record.walls.get(&window) {
        Some(old) if at < old.at => None,
        Some(old) if old.until == until => old.stands_until().map_or(Some(late), |end| {
            if at < end {
                None
            } else if at.saturating_sub(end) > WALL_EPISODE_S {
                Some(late)
            } else {
                Some(old.attempts.saturating_add(1))
            }
        }),
        _ => Some(late),
    };
    if let Some(attempts) = attempts {
        if !record.walls.contains_key(&window)
            && record.walls.len() >= MAX_WALLS
            && let Some(lapsed) = most_lapsed(&record.walls)
        {
            record.walls.remove(&lapsed);
        }
        record.walls.insert(
            window.clone(),
            Wall {
                until,
                at,
                by,
                attempts,
                cleared: false,
            },
        );
        let why = match until {
            Some(until) => format!("{window} until {until}"),
            None => format!("{window}, no reset named"),
        };
        out.push(Event::new(at, Kind::Wall, account).because(&why));
    }
    end_pin(state, account, "wall", at, &mut out);
    out
}

/// Records a settled `/usage` panel [`Sample`] for `account` at `now`.
pub fn note_sample(state: &mut State, account: &str, sample: Sample, now: i64) -> Vec<Event> {
    let by = state.machine.clone();
    let sample = Sample {
        session_pct: sample.session_pct.filter(|p| *p <= PCT_MAX),
        week_pct: sample.week_pct.filter(|p| *p <= PCT_MAX),
        session_read_at: sample
            .session_pct
            .filter(|p| *p <= PCT_MAX)
            .map(|_| sample.at),
        week_read_at: sample.week_pct.filter(|p| *p <= PCT_MAX).map(|_| sample.at),
        by,
        ..sample
    };
    if sample.session_pct.is_none() && sample.week_pct.is_none() {
        return Vec::new();
    }
    let Some(record) = account_mut(state, account) else {
        return Vec::new();
    };
    if record
        .sample
        .as_ref()
        .is_some_and(|held| sample.at < held.at.min(now) || sample.at == held.at)
    {
        return Vec::new();
    }
    let shown = |pct: Option<u32>| pct.map_or_else(|| "?".to_string(), |p| p.to_string());
    let event = Event::new(sample.at, Kind::Sample, account).because(&format!(
        "session {}% week {}%",
        shown(sample.session_pct),
        shown(sample.week_pct)
    ));
    record.before = record.sample.replace(sample);
    vec![event]
}

/// How long a usage tap's unchanged figure is held before it is written again ([`note_reading`]).
pub const READING_HEARTBEAT_S: i64 = 15 * 60;

/// How long after another account signed in to the home a running Claude Code may still be served
/// on the login it held ([`note_reading`], [`Account::keyed_at`]).
pub const KEY_ADOPT_S: i64 = 60;

fn sample_fig(pct: Option<u32>, reset: Option<i64>, at: i64, sample: &Sample) -> Option<WindowFig> {
    Some(WindowFig {
        pct_x10: pct.filter(|p| *p <= PCT_MAX)?.saturating_mul(10),
        reset: reset?,
        at,
        by: sample.by.clone(),
        source: SOURCE_SAMPLE.to_owned(),
        epoch_at: None,
    })
}

/// Converts a `/usage` [`Sample`] bucket ([`SESSION_LIMIT`] or [`WEEK_LIMIT`]) into a figure;
/// `None` when it has no figure there or `stored` already holds that figure as recently.
#[must_use]
pub fn sample_window(
    sample: Option<&Sample>,
    bucket: &str,
    stored: Option<&WindowFig>,
) -> Option<WindowFig> {
    let s = sample?;
    let f = match bucket {
        SESSION_LIMIT => sample_fig(
            s.session_pct,
            s.session_reset,
            s.session_read_at.unwrap_or(s.at),
            s,
        ),
        WEEK_LIMIT => sample_fig(s.week_pct, s.week_reset, s.week_read_at.unwrap_or(s.at), s),
        _ => None,
    }?;
    stored
        .is_none_or(|held| {
            (held.whole() * 10, held.reset) != (f.pct_x10, f.reset) || f.at > held.at
        })
        .then_some(f)
}

fn fig_news(f: Option<&WindowFig>) -> Option<(u32, i64, Option<i64>)> {
    f.map(|f| (f.pct_x10, f.reset, f.epoch_at))
}

/// Records a fresh usage-tap [`TapRead`] for `account` at `now`.
pub fn note_reading(state: &mut State, account: &str, read: &TapRead, now: i64) -> Vec<Event> {
    note_reading_held(state, account, read, now).0
}

/// Records a usage-tap [`TapRead`] and returns whether any part was held back during login
/// adoption.
pub fn note_reading_held(
    state: &mut State,
    account: &str,
    read: &TapRead,
    now: i64,
) -> (Vec<Event>, bool) {
    let valid = |w: Option<WindowRead>| w.filter(|w| w.pct_x10 <= 10 * PCT_MAX);
    let reads = [
        (SESSION_LIMIT, valid(read.five_hour)),
        (WEEK_LIMIT, valid(read.seven_day)),
    ];
    if reads.iter().all(|(_, w)| w.is_none()) {
        return (Vec::new(), false);
    }
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return (Vec::new(), false);
    };
    let at = read.at.min(now);
    let mut events = Vec::new();
    if record.of_another(read.key) {
        let Account {
            walls: _,
            sample: _,
            before: _,
            offer: _,
            auth,
            auth_at,
            cleared_at: _,
            ran_at,
            credits: _,
            credits_day,
            warned_session: _,
            warned_week: _,
            key: _,
            keyed_at: _,
            windows: _,
        } = std::mem::take(record);
        *record = Account {
            auth,
            auth_at,
            cleared_at: Some(now),
            keyed_at: Some(now),
            ran_at,
            credits_day,
            ..Account::default()
        };
        events.push(
            Event::new(now, Kind::Epoch, account).because("another account signed in to the home"),
        );
    }
    let mut changed = read.key.is_some() && record.key != read.key;
    if read.key.is_some() {
        record.key = read.key;
    }
    if record.adopting(at) {
        return (events, true);
    }
    let mut held_back = false;
    let sample = record.sample.clone();
    let mut figures: [Option<WindowFig>; 2] = [None, None];
    for (slot, (bucket, w)) in reads.into_iter().enumerate() {
        let stored = record.windows.get(bucket).cloned();
        let from_sample = sample_window(sample.as_ref(), bucket, stored.as_ref());
        let held = merge_fig(stored.as_ref(), from_sample.as_ref(), now);
        let anchored = |w: &WindowRead| held.as_ref().filter(|h| h.reset == w.reset);
        let fig = w.and_then(|w| {
            let epoch = anchored(&w).and_then(|h| h.epoch_at);
            if !read.seen_before && epoch.is_some_and(|e| read.born.is_none_or(|b| b < e)) {
                held_back = true;
                return None;
            }
            let refill = read.seen_before
                && w.refilled.is_some_and(|from| {
                    held.as_ref().is_some_and(|h| {
                        h.reset == w.reset
                            && h.epoch_at.is_none_or(|_| !h.cut() && from <= h.pct_x10)
                    })
                });
            Some(WindowFig {
                pct_x10: w.pct_x10,
                reset: w.reset,
                at,
                by: by.clone(),
                source: SOURCE_TAP.to_owned(),
                epoch_at: refill.then_some(at),
            })
        });
        let merged = merge_fig(held.as_ref(), fig.as_ref(), now);
        let opened = fig.as_ref().and_then(|f| f.epoch_at).is_some_and(|epoch| {
            merged.as_ref().and_then(|m| m.epoch_at) == Some(epoch)
                && w.and_then(|w| anchored(&w)).and_then(|h| h.epoch_at) < Some(epoch)
        });
        if opened {
            events.push(Event::new(now, Kind::Reset, account).because(&format!(
                "{bucket} refilled inside its window (resets {})",
                super::usage::rfc3339_utc(merged.as_ref().map_or(0, |m| m.reset))
            )));
        }
        if merged.is_some() {
            changed |= fig_news(merged.as_ref()) != fig_news(stored.as_ref());
        }
        figures[slot] = merged.or(stored);
    }
    let beat = sample
        .as_ref()
        .is_none_or(|s| now.saturating_sub(s.at) >= READING_HEARTBEAT_S);
    if !changed && !beat {
        return (events, held_back);
    }
    for ((bucket, _), fig) in reads.iter().zip(&figures) {
        if let Some(fig) = fig {
            record.windows.insert((*bucket).to_owned(), fig.clone());
        }
    }
    let live = |f: &Option<WindowFig>| {
        f.as_ref()
            .filter(|f| !f.cut())
            .map(|f| (f.whole(), f.reset))
    };
    let (session, week) = (live(&figures[0]), live(&figures[1]));
    let shown = |w: Option<(u32, i64)>| w.map_or_else(|| "?".to_string(), |(p, _)| p.to_string());
    events.push(Event::new(now, Kind::Sample, account).because(&format!(
        "tap: session {}% week {}%",
        shown(session),
        shown(week)
    )));
    // A heartbeat publishes now, but does not observe the held windows again. Keep a
    // newer matching panel observation too: equal values do not make it a heartbeat.
    let observed = |slot: usize, prior: Option<(u32, i64, i64)>| {
        let fig = figures[slot].as_ref().filter(|fig| !fig.cut())?;
        Some(
            prior
                .filter(|(pct, reset, _)| (*pct, *reset) == (fig.whole(), fig.reset))
                .map_or(fig.at, |(_, _, read_at)| fig.at.max(read_at)),
        )
    };
    let session_read_at = observed(
        0,
        sample.as_ref().and_then(|s| {
            Some((
                s.session_pct?,
                s.session_reset?,
                s.session_read_at.unwrap_or(s.at),
            ))
        }),
    );
    let week_read_at = observed(
        1,
        sample
            .as_ref()
            .and_then(|s| Some((s.week_pct?, s.week_reset?, s.week_read_at.unwrap_or(s.at)))),
    );
    let next = Sample {
        session_pct: session.map(|(p, _)| p),
        session_reset: session.map(|(_, r)| r),
        week_pct: week.map(|(p, _)| p),
        week_reset: week.map(|(_, r)| r),
        session_read_at,
        week_read_at,
        at: now,
        by,
    };
    record.before = record.sample.take();
    record.sample = Some(next);
    (events, held_back)
}

/// Records a limit-reset [`Offer`] observed at a wall on `account`.
pub fn note_offer(
    state: &mut State,
    account: &str,
    kind: Option<ResetKind>,
    left: u32,
    use_by: Option<i64>,
    at: i64,
    now: i64,
) -> bool {
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return false;
    };
    if record.offer.as_ref().is_some_and(|o| at < o.at.min(now)) {
        return false;
    }
    record.offer = Some(Offer {
        kind,
        left,
        use_by,
        at,
        by,
    });
    true
}

/// Records an [`Auth`] observation for `account` at `at`.
pub fn note_auth(state: &mut State, account: &str, auth: Auth, at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    if auth == Auth::Unseen {
        return out;
    }
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if record.auth_at.is_some_and(|held| at < held) {
        return out;
    }
    let changed = record.auth != auth;
    record.auth = auth;
    record.auth_at = Some(at);
    if changed {
        out.push(Event::new(at, Kind::Auth, account).because(auth.as_str()));
        if auth == Auth::SignedOut {
            end_pin(state, account, "signed-out", at, &mut out);
        }
    }
    out
}

/// Records the account `sid` is running on and any hold reason.
pub fn note_session(
    state: &mut State,
    sid: &str,
    account: &str,
    held: Option<&str>,
    now: i64,
) -> Vec<Event> {
    let sid = one_line(sid, WORD_MAX);
    if sid.is_empty() || !admissible_label(account) {
        return Vec::new();
    }
    let out = state
        .sessions
        .get(&sid)
        .filter(|old| old.account != account)
        .map(|old| {
            Event::new(now, Kind::Move, &old.account)
                .with_to(account)
                .with_sid(&sid)
        })
        .into_iter()
        .collect();
    if let Some(record) = account_mut(state, account) {
        record.ran_at = Some(record.ran_at.map_or(now, |held| held.max(now)));
    }
    state.sessions.insert(
        sid,
        SessionRow {
            account: account.to_owned(),
            at: now,
            held: held
                .map(|h| one_line(h, WORD_MAX))
                .filter(|h| !h.is_empty()),
        },
    );
    while state.sessions.len() > MAX_SESSIONS {
        let Some(oldest) = state
            .sessions
            .iter()
            .min_by(|a, b| a.1.at.cmp(&b.1.at).then_with(|| a.0.cmp(b.0)))
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        state.sessions.remove(&oldest);
    }
    out
}

/// Removes `sid` from the session table and clears its canary hold if it was the canary.
pub fn forget_session(state: &mut State, sid: &str) -> bool {
    if sid.is_empty() {
        return false;
    }
    let named = |c: &Option<Canary>| c.as_ref().is_some_and(|c| c.sid == sid);
    let canary = named(&state.canary);
    let passed = state.undo.iter().rposition(|over| named(&over.canary));
    if canary {
        state.canary = None;
        state.undo.clear();
    } else if let Some(k) = passed {
        state.undo.drain(..k);
        if let Some(over) = state.undo.first_mut() {
            over.canary = None;
        }
    }
    state.sessions.remove(sid).is_some() || canary || passed.is_some()
}

/// Removes session rows not updated within [`SESSION_KEEP_S`] of `now`.
pub fn drop_stale_sessions(state: &mut State, now: i64) -> usize {
    let before = state.sessions.len();
    state
        .sessions
        .retain(|_, row| now.saturating_sub(row.at) <= SESSION_KEEP_S);
    before - state.sessions.len()
}

/// Returns whether an existing probe claim prevents claiming a new `/usage` probe for `account` at
/// `now`.
#[must_use]
pub fn probe_held(state: &State, account: &str, now: i64) -> bool {
    state.probe.as_ref().is_some_and(|c| {
        let within = if c.account == account {
            PROBE_MIN_S
        } else {
            PROBE_APART_S
        };
        recent(c.at, now, within)
    })
}

/// Claims the `/usage` probe slot for `account` and `sid` at `now` if unheld.
pub fn claim_probe(state: &mut State, account: &str, sid: &str, now: i64) -> Option<Event> {
    let sid = one_line(sid, WORD_MAX);
    if sid.is_empty() || !admissible_label(account) {
        return None;
    }
    if probe_held(state, account, now) {
        return None;
    }
    state.probe = Some(ProbeClaim {
        account: account.to_owned(),
        sid: sid.clone(),
        at: now,
    });
    Some(Event::new(now, Kind::Probe, account).with_sid(&sid))
}

/// Outcome of attempting to claim a limit-reset slot via [`claim_reset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetClaimed {
    /// The slot is this session's from now.
    Taken,
    /// It already was: this session and this process claimed this wall.
    Mine,
    /// A claim is in flight that is not this session's own for this wall ([`RESET_CLAIM_S`]).
    Held {
        /// When that claim lapses.
        until: i64,
    },
    /// This wall was attempted already and is not attempted again.
    Attempted,
}

/// Attempts to claim the single limit-reset slot for `account`'s standing wall.
pub fn claim_reset(
    state: &mut State,
    account: &str,
    wall: (&str, i64),
    sid: &str,
    pid: u32,
    start: &str,
    now: i64,
) -> ResetClaimed {
    let (window, until) = wall;
    let sid = one_line(sid, WORD_MAX);
    let start = one_line(start, WORD_MAX);
    let window = one_line(window, WORD_MAX);
    if sid.is_empty()
        || start.is_empty()
        || window.is_empty()
        || pid == 0
        || !admissible_label(account)
    {
        return ResetClaimed::Attempted;
    }
    if let Some(held) = &state.reset {
        let flying = held.done.is_none() && recent(held.at, now, RESET_CLAIM_S);
        let this_wall = held.until == Some(until);
        let mine = held.sid == sid
            && held.pid == pid
            && held.start == start
            && held.window.as_deref() == Some(window.as_str());
        let stands_past = held
            .until
            .is_some_and(|theirs| until < theirs && now < theirs);
        if flying && this_wall && mine {
            return ResetClaimed::Mine;
        }
        if flying {
            return ResetClaimed::Held {
                until: held.at.saturating_add(RESET_CLAIM_S),
            };
        }
        if this_wall || stands_past {
            return ResetClaimed::Attempted;
        }
    }
    state.reset = Some(ResetClaim {
        account: account.to_owned(),
        until: Some(until),
        window: Some(window),
        sid,
        at: now,
        pid,
        start,
        done: None,
    });
    ResetClaimed::Taken
}

/// Returns the claim expiration timestamp if a limit reset is currently in flight on `account` at
/// `now`.
#[must_use]
pub fn reset_in_flight(state: &State, account: &str, now: i64) -> Option<i64> {
    state
        .reset
        .as_ref()
        .filter(|c| c.account == account && c.done.is_none() && recent(c.at, now, RESET_CLAIM_S))
        .map(|c| c.at.saturating_add(RESET_CLAIM_S))
}

/// Returns whether a process on `label` started at `started` is eligible to claim a limit reset.
/// # Errors
///
/// The first check that fails, by its word.
pub fn reset_allowed(
    roster: &Roster,
    state: &State,
    label: &str,
    started: i64,
) -> Result<(), &'static str> {
    let Some(row) = roster.row(label) else {
        return Err("not-listed");
    };
    if row.dir.is_some() {
        return Err("not-default");
    }
    if state
        .active
        .as_deref()
        .is_some_and(|active| active != label)
    {
        return Err("active-other");
    }
    let floor = started.saturating_sub(RESET_PROFILE_S);
    let ran = state
        .accounts
        .iter()
        .filter_map(|(other, record)| record.ran_at.map(|at| (other.as_str(), at)))
        .chain(
            state
                .sessions
                .values()
                .map(|session| (session.account.as_str(), session.at)),
        );
    for (other, at) in ran {
        if other != label && at > floor {
            return Err("other-login");
        }
    }
    Ok(())
}

/// Outcome of a claimed limit-reset attempt passed to [`reset_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetDone<'a> {
    /// The vendor's own line said the limits were reset.
    Used {
        /// The kind spent.
        kind: ResetKind,
        /// Resets of that kind left after this one.
        left: Option<u32>,
    },
    /// The vendor's own line said a reset was spent and the walled limit still stands.
    Walled {
        /// The kind spent.
        kind: ResetKind,
        /// Resets of that kind left after this one.
        left: u32,
    },
    /// The vendor said the limit had lifted by itself.
    Lifted,
    /// The reset was kept, for this reason (one word of the caller's).
    Kept(&'a str),
    /// The attempt failed or its end was not read, for this reason.
    Failed(&'a str),
}

impl ResetDone<'_> {
    /// Canonical status word stored in [`ResetClaim::done`].
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            ResetDone::Used { .. } | ResetDone::Walled { .. } => "used",
            ResetDone::Lifted => "lifted",
            ResetDone::Kept(_) => "kept",
            ResetDone::Failed(_) => "failed",
        }
    }
}

fn reset_reason(why: &str) -> String {
    let line = one_line(why, WORD_MAX);
    let head = line.split(" · ").next().unwrap_or_default();
    let end = head.find(['/', '\\', '@', '~']).unwrap_or(head.len());
    head.get(..end).unwrap_or_default().trim().to_owned()
}

/// Records the outcome of a claimed limit-reset attempt on `account` at `now`.
pub fn reset_outcome(
    state: &mut State,
    account: &str,
    until: i64,
    sid: &str,
    done: &ResetDone<'_>,
    now: i64,
) -> Vec<Event> {
    let sid = one_line(sid, WORD_MAX);
    let Some(claim) = state.reset.as_mut().filter(|c| {
        c.account == account && c.until == Some(until) && c.sid == sid && c.done.is_none()
    }) else {
        return Vec::new();
    };
    claim.done = Some(done.word().to_owned());
    let claimed = claim.at;
    let window = claim.window.clone();
    let machine = state.machine.clone();
    let (kind, why) = match done {
        ResetDone::Used { kind, left } => (
            Kind::ResetUsed,
            left.map_or_else(
                || kind.name().to_owned(),
                |n| format!("{} left", kind.resets(n)),
            ),
        ),
        ResetDone::Walled { kind, left } => (
            Kind::ResetUsed,
            format!("{} left, still walled", kind.resets(*left)),
        ),
        ResetDone::Lifted => (Kind::ResetKept, ResetDone::Lifted.word().to_owned()),
        ResetDone::Kept(why) => (Kind::ResetKept, reset_reason(why)),
        ResetDone::Failed(why) => (Kind::ResetFailed, reset_reason(why)),
    };
    if matches!(done, ResetDone::Used { .. } | ResetDone::Lifted)
        && let Some(window) = window
        && let Some(record) = account_mut(state, account)
        && let Some(wall) = record.walls.get_mut(&window)
        && wall.until == Some(until)
        && wall.at <= claimed
        && !wall.cleared
    {
        let used = matches!(done, ResetDone::Used { .. });
        let stood = wall.stands_until().is_some_and(|end| claimed < end);
        wall.cleared = true;
        wall.at = now;
        wall.by = machine;
        wall.attempts = 0;
        let sample = record.sample.as_ref();
        let bucket = match window.as_str() {
            WEEK_LIMIT => Some((
                &mut record.warned_week,
                sample.filter(|s| s.week_pct.is_some()).map(|s| s.at),
            )),
            SESSION_LIMIT => Some((
                &mut record.warned_session,
                sample.filter(|s| s.session_pct.is_some()).map(|s| s.at),
            )),
            _ => None,
        };
        if stood
            && let Some((warned, sampled)) = bucket
            && sampled.is_none_or(|at| at <= claimed)
            && warned.as_ref().is_none_or(|w| w.at <= claimed)
        {
            *warned = Some(WindowSaid {
                pct: None,
                reset: (!used).then_some(until),
                at: now,
            });
        }
        if used
            && stood
            && let Some(fig) = record.windows.get_mut(window.as_str())
        {
            fig.epoch_at = fig.epoch_at.max(Some(now));
        }
    }
    vec![Event::new(now, kind, account).with_sid(&sid).because(&why)]
}

/// Returns whether the machine's hourly account-rotation circuit breaker is spent at `now`.
#[must_use]
pub fn rotations_spent(state: &State, roster: &Roster, now: i64) -> bool {
    let counted = state
        .rotations
        .iter()
        .filter(|t| recent(**t, now, ROTATION_WINDOW_S))
        .count();
    counted >= roster.rows.len().saturating_mul(2)
}

/// Returns the timestamp when a spent rotation circuit breaker reopens.
#[must_use]
pub fn breaker_until(state: &State, roster: &Roster, now: i64) -> Option<i64> {
    let bound = roster.rows.len().saturating_mul(2);
    if bound == 0 {
        return None;
    }
    let mut counted: Vec<i64> = state
        .rotations
        .iter()
        .copied()
        .filter(|t| recent(*t, now, ROTATION_WINDOW_S))
        .collect();
    counted.sort_unstable();
    let over = counted.len().checked_sub(bound)?;
    counted
        .get(over)
        .map(|t| t.saturating_add(ROTATION_WINDOW_S))
}

/// Reason a compare-and-set rotation in [`begin_rotation`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lost {
    /// `active` is not what the caller read.
    Raced {
        /// What `active` is.
        active: Option<String>,
    },
    /// `active` as read can be run on and is not rotated past.
    Held {
        /// That account.
        active: String,
    },
    /// The owner's pin holds.
    Pinned,
    /// `to` is not a row, is the session's own or the active account, or cannot be run on now.
    NotACandidate,
    /// The machine's rotation bound is spent.
    Spent,
    /// The mover has no session id.
    NoSession,
}

/// Session and expected state snapshot for [`begin_rotation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mover<'a> {
    /// Its session id: the canary's, once the rotation is committed.
    pub sid: &'a str,
    /// The account its process runs on.
    pub account: &'a str,
}

/// Commits an atomic compare-and-set rotation moving `mover` onto candidate account `to`
/// ([`Lost`]).
/// # Errors
///
/// A [`Lost`], with nothing changed.
pub fn begin_rotation(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    expect_active: Option<&str>,
    mover: Mover<'_>,
    to: &str,
    now: i64,
) -> Result<Event, Lost> {
    let sid = one_line(mover.sid, WORD_MAX);
    if sid.is_empty() {
        return Err(Lost::NoSession);
    }
    if state.active.as_deref() != expect_active {
        return Err(Lost::Raced {
            active: state.active.clone(),
        });
    }
    let me = mover.account;
    let cond = |label: &str| {
        roster
            .row(label)
            .map(|r| condition(r, state, is_enrolled(r, enrolled), now))
    };
    let runs = |label: &str| cond(label).is_some_and(Condition::runs);
    let fresh = |label: &str| cond(label).is_some_and(Condition::fresh);
    let active_runs = expect_active.is_some_and(runs);
    if let Some(active) = expect_active
        && active_runs
    {
        let held = Err(Lost::Held {
            active: active.to_owned(),
        });
        if !state.proven {
            return held;
        }
        if state.pinned {
            return if active == me {
                Err(Lost::Pinned)
            } else {
                held
            };
        }
        if active != me && (fresh(active) || !fresh(to)) {
            return held;
        }
    }
    if to == me || expect_active == Some(to) || !runs(to) {
        return Err(Lost::NotACandidate);
    }
    if rotations_spent(state, roster, now) {
        return Err(Lost::Spent);
    }
    let from = expect_active.unwrap_or(me).to_owned();
    let proven = state.active.is_some() && state.proven;
    let over = Undo {
        previous: std::mem::replace(&mut state.previous, state.active.take()),
        previous_proven: std::mem::replace(&mut state.previous_proven, proven),
        canary: state.canary.replace(Canary {
            sid: sid.clone(),
            at: now,
            took: false,
        }),
    };
    state.undo.push(over);
    let extra = state.undo.len().saturating_sub(MAX_UNDO);
    state.undo.drain(..extra);
    state.active = Some(to.to_owned());
    state.proven = false;
    state.pinned = false;
    state
        .rotations
        .retain(|t| recent(*t, now, ROTATION_WINDOW_S));
    state.rotations.push(now);
    let extra = state.rotations.len().saturating_sub(MAX_ROTATIONS);
    state.rotations.drain(..extra);
    Ok(Event::new(now, Kind::Rotate, &from)
        .with_to(to)
        .with_sid(&sid))
}

/// Replaces the timed-out canary `expect` on `active` with `sid`, by compare-and-set.
pub fn take_canary(
    state: &mut State,
    expect: Option<&Canary>,
    sid: &str,
    now: i64,
) -> Option<Event> {
    let sid = one_line(sid, WORD_MAX);
    let active = state.active.clone()?;
    if sid.is_empty() || state.proven || state.canary.as_ref() != expect || state.canary_holds(now)
    {
        return None;
    }
    let silent = state.canary.replace(Canary {
        sid: sid.clone(),
        at: now,
        took: true,
    });
    state.undo.clear();
    let why = match silent {
        Some(silent) if !silent.sid.is_empty() => {
            format!("canary: in place of {}, which did not answer", silent.sid)
        }
        _ => "canary: no session held it".to_string(),
    };
    Some(
        Event::new(now, Kind::Rotate, &active)
            .with_to(&active)
            .with_sid(&sid)
            .because(&why),
    )
}

/// Marks `account` as proven when it serves a response, releasing any canary hold.
pub fn prove(state: &mut State, account: &str, now: i64) -> Option<Event> {
    if state.active.as_deref() != Some(account) || state.proven {
        return None;
    }
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    if let Some(record) = account_mut(state, account) {
        record.auth = Auth::Ok;
        record.auth_at = Some(now);
    }
    Some(Event::new(now, Kind::Proven, account))
}

/// Failure reason when a canary session on a newly rotated account fails to serve a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strike {
    /// It answered with a usage wall of its own.
    Wall {
        /// The vendor's limit type.
        window: String,
        /// The reset it named.
        until: Option<i64>,
    },
    /// It answered with a login wall.
    SignedOut,
    /// The move failed with no word about the account.
    Failed(String),
    /// The move was never made.
    NotMoved(String),
}

/// Strikes candidate `account` on behalf of `sid`. A strike of the unproven `active` rolls it
/// back to [`State::previous`] ([`State::undo`]), except a [`Strike::NotMoved`] of a canary that
/// took a silent one's place; a [`Strike::NotMoved`] counts only from that account's own canary.
pub fn strike(state: &mut State, account: &str, sid: &str, why: &Strike, now: i64) -> Vec<Event> {
    if !admissible_label(account) {
        return Vec::new();
    }
    let sid = one_line(sid, WORD_MAX);
    let canary = state.active.as_deref() == Some(account) && !state.proven;
    let not_moved = matches!(why, Strike::NotMoved(_));
    let its_own = !sid.is_empty() && state.canary.as_ref().is_some_and(|c| c.sid == sid);
    if not_moved && !(canary && its_own) {
        return Vec::new();
    }
    let word = match why {
        Strike::Wall { window, .. } => format!("wall: {window}"),
        Strike::SignedOut => "signed-out".to_string(),
        Strike::Failed(what) => format!("failed: {what}"),
        Strike::NotMoved(what) => format!("not moved: {what}"),
    };
    let mut struck = Event::new(now, Kind::Struck, account)
        .with_sid(&sid)
        .because(&word);
    if canary {
        let held = state.canary.take();
        let took = held.as_ref().is_some_and(|c| c.took);
        let undoes = not_moved && !took;
        if undoes || !not_moved {
            state.active = state.previous.take();
            state.proven = state.active.is_some() && state.previous_proven;
            state.previous_proven = false;
            state.pinned = false;
            struck.to.clone_from(&state.active);
            if let Some(over) = state.undo.pop() {
                state.previous = over.previous;
                state.previous_proven = over.previous_proven;
                state.canary = over.canary;
            }
        }
        if undoes {
            let own = held.and_then(|c| state.rotations.iter().rposition(|t| *t == c.at));
            match own {
                Some(k) => {
                    state.rotations.remove(k);
                }
                None => {
                    state.rotations.pop();
                }
            }
        }
    }
    let mut out = vec![struck];
    out.extend(match why {
        Strike::Wall { window, until } => note_wall(state, account, window, *until, now, now),
        Strike::SignedOut => note_auth(state, account, Auth::SignedOut, now),
        Strike::Failed(_) => note_wall(state, account, STRUCK_WINDOW, None, now, now),
        Strike::NotMoved(_) => Vec::new(),
    });
    out
}

#[derive(Clone, Copy)]
enum PinRefusal {
    NotListed,
    NotEnrolled,
    SignedOut,
}

impl PinRefusal {
    fn message(self, label: &str) -> String {
        match self {
            Self::NotListed => format!("`{}` is not in the roster", shown(label)),
            Self::NotEnrolled => format!(
                "`{label}` is not-enrolled: no login was made for it on this machine (its login \
                 directory is missing or empty)"
            ),
            Self::SignedOut => format!("`{label}` is signed-out"),
        }
    }
}

fn pin_refusal(
    state: &State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    label: &str,
) -> Option<PinRefusal> {
    let Some(row) = roster.row(label) else {
        return Some(PinRefusal::NotListed);
    };
    if !is_enrolled(row, enrolled) {
        return Some(PinRefusal::NotEnrolled);
    }
    state
        .accounts
        .get(label)
        .is_some_and(|a| a.auth == Auth::SignedOut)
        .then_some(PinRefusal::SignedOut)
}

/// What an explicit account selection saved under the store lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinSelection {
    /// The account is eligible and is now pinned.
    Pinned,
    /// The account needs sign-in; this exact selection is saved for its completion.
    Renew(PinRequest),
}

/// Pins `label` as the active account (`accounts use <label>`) and clears its standing walls.
/// # Errors
///
/// By name: a label the roster does not list, one not enrolled on this
/// machine, one known signed out. Nothing is changed.
pub fn pin(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    label: &str,
    now: i64,
) -> Result<Event, String> {
    if let Some(why) = pin_refusal(state, roster, enrolled, label) {
        return Err(why.message(label));
    }
    state.pending_pin = None;
    if let Some(record) = account_mut(state, label) {
        record.walls.clear();
        record.cleared_at = Some(now);
    }
    if state.active.as_deref() != Some(label) {
        state.previous_proven = state.active.is_some() && state.proven;
        state.previous = state.active.take();
        state.active = Some(label.to_owned());
    }
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    state.pinned = true;
    Ok(Event::new(now, Kind::Use, label))
}

/// Clears an operator account pin (`accounts use --auto`).
pub fn unpin(state: &mut State, why: &str, now: i64) -> Option<Event> {
    state.pending_pin = None;
    if !state.pinned {
        return None;
    }
    state.pinned = false;
    let account = state.active.clone().unwrap_or_default();
    Some(Event::new(now, Kind::Unpin, &account).because(why))
}

/// Completes only the explicit selection that still owns the pending request.
pub(super) fn complete_requested_pin(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    request: &PinRequest,
    now: i64,
) -> Option<Event> {
    if state.pending_pin.as_ref() != Some(request) {
        return None;
    }
    pin(state, roster, enrolled, request.label(), now).ok()
}

const LOCK_FILE: &str = "accounts.lock";

const STATE_FILE: &str = "state.json";

const LEDGER_FILE: &str = "ledger.jsonl";

const WAKE_FILE: &str = "wake";

const MACHINE_FILE: &str = "machine";

fn is_machine_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= WORD_MAX && aterm_codec::hex::is_hex(id, id.len())
}

fn absent_at_open(look: io::Result<std::fs::Metadata>) -> bool {
    match look {
        Err(e) => e.kind() == io::ErrorKind::NotFound,
        Ok(meta) => !meta.file_type().is_symlink(),
    }
}

/// Reads at most `max` bytes from a regular file at `path`, returning `Ok(None)` if absent.
pub(crate) fn read_bounded(path: &Path, max: u64) -> io::Result<Option<String>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return if absent_at_open(std::fs::symlink_metadata(path)) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("larger than {max} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// On-disk store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// The directory (0700).
    pub dir: PathBuf,
    /// The root of its rows' login directories ([`Store::logins_dir`]).
    logins: PathBuf,
    /// The pauses of its bounded lock takes ([`LOCK_LADDER`], [`Store::signin_within`]).
    ladder: &'static [std::time::Duration],
    /// The CLI whose pool it holds ([`Store::at_for`]).
    cli: Cli,
}

/// The machine-wide sign-in lock's file, in Claude Code's store ([`Store::signin_within`]).
pub const SIGNIN_LOCK_FILE: &str = "signin.lock";

/// What a sign-in that finds [`SIGNIN_LOCK_FILE`] held past its ladder reports.
pub const SIGNIN_HELD: &str = "another sign-in holds the machine's sign-in lock";

/// Exclusive `flock(2)` guard on a store lock file (`accounts.lock`, [`SIGNIN_LOCK_FILE`]) that
/// unlocks on drop.
#[derive(Debug)]
pub struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Store {
    /// Opens the accounts [`Store`] rooted under `harness_state`.
    #[must_use]
    pub fn at(harness_state: &Path) -> Store {
        let dir = harness_state.join("accounts");
        Store {
            logins: dir.join("logins"),
            dir,
            ladder: &LOCK_LADDER,
            cli: Cli::Claude,
        }
    }

    /// Opens `cli`'s [`Store`] ([`Cli`]): Claude Code's is [`Store::at`]; another CLI's is
    /// `<harness_state>/<store_dir>`, its rows' logins under `<user_home>/.aterm/logins/<cli>`,
    /// a path that names the CLI and that its own sandbox lets it read.
    #[must_use]
    pub fn at_for(harness_state: &Path, cli: Cli, user_home: &Path) -> Store {
        match cli {
            Cli::Claude => Store::at(harness_state),
            _ => Store {
                dir: harness_state.join(cli.accounts().store_dir),
                logins: user_home.join(".aterm").join("logins").join(cli.id()),
                ladder: &LOCK_LADDER,
                cli,
            },
        }
    }

    /// The CLI whose pool it holds.
    #[must_use]
    pub const fn cli(&self) -> Cli {
        self.cli
    }

    /// The CLI's own default home in the user's home that holds this store's logins
    /// (`<home>/.aterm/logins/<cli>`, [`Store::at_for`]); `None` for Claude Code's store.
    #[must_use]
    pub fn default_home(&self) -> Option<PathBuf> {
        if self.cli == Cli::Claude {
            return None;
        }
        let user_home = self.logins.parent()?.parent()?.parent()?;
        Some(user_home.join(self.cli.accounts().default_home))
    }

    /// Claude Code's store directory beside it (the same directory for Claude Code's own): the
    /// one root every CLI's sign-ins share, for the browser profiles (one per email for the
    /// vendors, Claude Code's own shared where it is keyed by that exact email,
    /// [`super::headless_oauth::vendor_profile_dir`], so one account's mailbox session serves
    /// every vendor until a window's tidy drops its identity providers' cookies, a Microsoft
    /// webmail's among them, [`super::browser_window::tidy_profile`]) and the sign-in lock
    /// ([`Store::signin_within`]).
    #[must_use]
    pub fn shared_dir(&self) -> PathBuf {
        match self.cli {
            Cli::Claude => self.dir.clone(),
            _ => self
                .dir
                .parent()
                .map_or_else(|| self.dir.clone(), |state| state.join("accounts")),
        }
    }

    /// Takes the machine-wide sign-in lock ([`SIGNIN_LOCK_FILE`] in [`Store::shared_dir`]) on
    /// this store's ladder: one sign-in at a time on this machine, for every CLI, since a
    /// headless browser's profile admits one process and Codex's login listens on one fixed
    /// port. `None` when it stayed held past the ladder.
    /// # Errors
    /// The directory or the lock file cannot be made, or the lock failed.
    pub fn signin_within(&self) -> io::Result<Option<Lock>> {
        self.signin_on(self.ladder)
    }

    /// [`Store::signin_within`] tried once, never waited for: `None` while another sign-in
    /// holds it.
    /// # Errors
    /// As [`Store::signin_within`].
    pub fn signin_now(&self) -> io::Result<Option<Lock>> {
        self.signin_on(&[])
    }

    fn signin_on(&self, ladder: &[std::time::Duration]) -> io::Result<Option<Lock>> {
        let shared = self.shared_dir();
        aterm_types::fs_restricted::ensure_private_dir(&shared)?;
        let mut open = std::fs::OpenOptions::new();
        open.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        let mut file = match Self::try_take(open.open(shared.join(SIGNIN_LOCK_FILE))?)? {
            Ok(lock) => return Ok(Some(lock)),
            Err(file) => file,
        };
        for pause in ladder {
            std::thread::sleep(*pause);
            file = match Self::try_take(file)? {
                Ok(lock) => return Ok(Some(lock)),
                Err(file) => file,
            };
        }
        Ok(None)
    }

    /// Overrides the ladder of this store's bounded lock takes.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn bounded_by(self, ladder: &'static [std::time::Duration]) -> Store {
        Store { ladder, ..self }
    }

    /// Ensures the store directory exists with mode `0700` owned by the current user.
    /// # Errors
    /// It cannot be made, or it is not this user's private directory.
    pub fn ensure(&self) -> io::Result<()> {
        aterm_types::fs_restricted::ensure_private_dir(&self.dir)
    }

    fn lock_file(&self) -> io::Result<std::fs::File> {
        self.ensure()?;
        let mut open = std::fs::OpenOptions::new();
        open.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        open.open(self.dir.join(LOCK_FILE))
    }

    /// Acquires `accounts.lock`, blocking until the lock is available.
    /// # Errors
    /// The directory or the lock file cannot be made, or the lock failed.
    pub fn acquire(&self) -> io::Result<Lock> {
        let file: std::fs::File = self.lock_file()?;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.lock();
            match taken {
                Ok(()) => return Ok(Lock(file)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Attempts a non-blocking acquire of `accounts.lock` ([`Store::acquire`]).
    /// # Errors
    /// As [`Store::acquire`].
    pub fn try_acquire(&self) -> io::Result<Option<Lock>> {
        Ok(Self::try_take(self.lock_file()?)?.ok())
    }

    fn try_take(file: std::fs::File) -> io::Result<Result<Lock, std::fs::File>> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Ok(Lock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(Err(file)),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Tries `accounts.lock` now and after each pause of `ladder`; `None` when it stayed held.
    /// # Errors
    /// As [`Store::acquire`].
    pub fn acquire_within(&self, ladder: &[std::time::Duration]) -> io::Result<Option<Lock>> {
        let mut file = match Self::try_take(self.lock_file()?)? {
            Ok(lock) => return Ok(Some(lock)),
            Err(file) => file,
        };
        for pause in ladder {
            std::thread::sleep(*pause);
            file = match Self::try_take(file)? {
                Ok(lock) => return Ok(Some(lock)),
                Err(file) => file,
            };
        }
        Ok(None)
    }

    /// Path to `state.json`.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// Path to `ledger.jsonl`.
    #[must_use]
    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join(LEDGER_FILE)
    }

    /// Path to the directory holding per-account login directories: `logins/` in the store, or a
    /// CLI's own root ([`Store::at_for`]).
    #[must_use]
    pub fn logins_dir(&self) -> PathBuf {
        self.logins.clone()
    }

    /// Path to the login directory for `label`, or `None` if `label` is not admissible.
    #[must_use]
    pub fn login_dir(&self, label: &str) -> Option<PathBuf> {
        admissible_label(label).then(|| self.logins_dir().join(label))
    }

    /// Path to the `wake` marker file.
    #[must_use]
    pub fn wake_path(&self) -> PathBuf {
        self.dir.join(WAKE_FILE)
    }

    /// Path to `logins.json`.
    #[must_use]
    pub fn logins_file(&self) -> PathBuf {
        self.dir.join(LOGINS_FILE)
    }

    /// Creates a fresh `0700` staging directory `<label>+<n>` in the logins directory.
    /// # Errors
    /// A label no roster could carry; the logins directory or the staging
    /// directory cannot be made; every name up to `<label>+9999` is taken.
    pub fn claim_staging_dir(&self, label: &str) -> io::Result<PathBuf> {
        if !admissible_label(label) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a label a roster could carry",
            ));
        }
        let logins = self.logins_dir();
        aterm_types::fs_restricted::ensure_private_dir(&logins)?;
        for n in 1..=9999 {
            let dir = logins.join(format!("{label}+{n}"));
            #[cfg(unix)]
            let made = {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&dir)
            };
            #[cfg(not(unix))]
            let made = std::fs::create_dir(&dir);
            match made {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("every staging name is taken"))
    }

    /// Loads `logins.json` as of `now`; an absent file holds no bindings.
    /// # Errors
    /// A file that cannot be read, is larger than [`MAX_LOGINS_BYTES`], is not UTF-8, or that
    /// [`Logins::from_json`] refuses (not JSON, or JSON of another schema or of none).
    pub fn load_logins(&self, now: i64) -> Result<(Logins, Option<String>), String> {
        match read_bounded(&self.logins_file(), MAX_LOGINS_BYTES) {
            Ok(None) => Ok((Logins::default(), None)),
            Ok(Some(text)) => match Logins::from_json(&text, now) {
                Ok(logins) => Ok((logins, None)),
                Err(Unread::Torn(why) | Unread::NotOurs(why)) => {
                    Err(format!("{LOGINS_FILE}: {why}"))
                }
            },
            Err(e) => Err(format!("{LOGINS_FILE}: {e}")),
        }
    }

    /// Loads `logins.json` and returns `roster` with machine-local renewal bindings applied.
    /// # Errors
    /// As [`Self::load_logins`].
    pub fn bound(&self, roster: &Roster, now: i64) -> Result<Roster, String> {
        self.load_logins(now)
            .map(|(logins, _)| bound(roster, &logins))
    }

    /// Atomically writes `logins.json` (`0600`) via a sibling temporary file.
    /// # Errors
    /// The directory cannot be made or the write or rename failed.
    pub fn save_logins(&self, logins: &Logins) -> io::Result<()> {
        self.ensure()?;
        let mut text = logins.to_json();
        text.push('\n');
        super::upgrade_models::write_atomic(&self.logins_file(), &text)
    }

    /// Atomically replaces the `wake` marker file so watchers detect an inode change.
    /// # Errors
    /// The directory cannot be made or the write or rename failed.
    pub fn touch_wake(&self) -> io::Result<()> {
        self.ensure()?;
        super::upgrade_models::write_atomic_ordered(&self.wake_path(), "")
    }

    /// Loads `state.json` as of `now`; an absent, torn or non-UTF-8 file reads as an empty state.
    /// # Errors
    /// A file that is not ours to replace, named: one that could not be read (a permission, an
    /// I/O error, a directory in its place), one larger than [`MAX_STATE_BYTES`], or JSON of
    /// another schema or none ([`Unread::NotOurs`]). It may hold a pin, walls and signed-out
    /// words, so a transaction that gets this answer stops without saving, and a read-only
    /// caller shows an empty state beside the reason.
    pub fn load(&self, now: i64) -> Result<(State, Option<String>), String> {
        let (mut state, mut note) = match read_bounded(&self.state_file(), MAX_STATE_BYTES) {
            Ok(None) => (State::default(), None),
            Ok(Some(text)) => match State::from_json(&text, now) {
                Ok(state) => (state, None),
                Err(Unread::Torn(why)) => (
                    State::default(),
                    Some(format!("{STATE_FILE}: {why}; read as an empty state")),
                ),
                Err(Unread::NotOurs(why)) => return Err(format!("{STATE_FILE}: {why}")),
            },
            Err(e) if e.kind() == io::ErrorKind::InvalidData => (
                State::default(),
                Some(format!("{STATE_FILE}: {e}; read as an empty state")),
            ),
            Err(e) => return Err(format!("{STATE_FILE}: {e}")),
        };
        match self.kept_machine() {
            Ok(Some(id)) => state.machine = id,
            Ok(None) if !state.machine.is_empty() => {}
            Ok(None) => match mint_machine() {
                Ok(id) => state.machine = id,
                Err(e) => {
                    note.get_or_insert_with(|| {
                        format!("no machine id could be minted ({e}); observations carry none")
                    });
                }
            },
            Err(e) => {
                state.machine.clear();
                note.get_or_insert_with(|| {
                    format!("no machine id could be read ({e}); observations carry none")
                });
            }
        }
        Ok((state, note))
    }

    /// Path to the `<dir>/machine` identifier file.
    #[must_use]
    pub fn machine_file(&self) -> PathBuf {
        self.dir.join(MACHINE_FILE)
    }

    /// The id the store keeps ([`Self::machine_file`]), where it keeps one.
    /// # Errors
    /// The file is there and could not be read, or holds no id.
    fn kept_machine(&self) -> io::Result<Option<String>> {
        match read_bounded(&self.machine_file(), 64)? {
            None => Ok(None),
            Some(text) if is_machine_id(text.trim()) => Ok(Some(text.trim().to_owned())),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a machine id",
            )),
        }
    }

    /// Reads or mints and persists this machine's 16-hex-char identifier.
    /// # Errors
    /// The directory cannot be made, the file holds no id, or the write or
    /// the link failed.
    pub fn machine_id(&self, held: &str) -> io::Result<String> {
        if let Some(id) = self.kept_machine()? {
            return Ok(id);
        }
        let id = if is_machine_id(held) {
            held.to_owned()
        } else {
            mint_machine()?
        };
        self.ensure()?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".machine-")
            .tempdir_in(&self.dir)?;
        self.keep_machine_at(&id, &stage.path().join("id"))?;
        self.kept_machine()?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "kept, then gone"))
    }

    fn keep_machine_at(&self, id: &str, tmp: &Path) -> io::Result<()> {
        use std::io::Write as _;
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        let mut file = open.open(tmp)?;
        let written = (|| {
            file.write_all(format!("{id}\n").as_bytes())?;
            aterm_durable::durable(&file)?;
            match std::fs::hard_link(tmp, self.machine_file()) {
                Ok(()) => aterm_durable::durable_dir(&self.dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(e) => Err(e),
            }
        })();
        drop(file);
        let _ = std::fs::remove_file(tmp);
        written
    }

    /// Atomically writes `state.json` (`0600`) via a sibling temporary file.
    /// # Errors
    /// The directory cannot be made or the write or rename failed.
    pub fn save(&self, state: &State) -> io::Result<()> {
        self.ensure()?;
        let mut text = state.to_json();
        text.push('\n');
        super::upgrade_models::write_atomic(&self.state_file(), &text)
    }

    /// Appends `events` to `ledger.jsonl` ([`LEDGER_MAX_BYTES`]).
    /// # Errors
    /// The directory cannot be made or the append or the cut failed.
    pub fn append(&self, events: &[Event]) -> io::Result<()> {
        use std::io::Write as _;
        if events.is_empty() {
            return Ok(());
        }
        self.ensure()?;
        let mut text = String::new();
        for event in events {
            text.push_str(&event.to_json_line());
            text.push('\n');
        }
        let path = self.ledger_path();
        let mut open = std::fs::OpenOptions::new();
        open.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        let mut file = open.open(&path)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS).map(|_| ())
    }

    /// Reads the newest `max` events from `ledger.jsonl` in chronological order.
    /// # Errors
    /// The ledger exists and cannot be read.
    pub fn tail(&self, max: usize) -> io::Result<Vec<Event>> {
        Ok(super::cli::tail_lines(&self.ledger_path(), max)?
            .iter()
            .filter_map(|line| Event::of_json_line(line))
            .collect())
    }

    /// Runs an atomic read-modify-write transaction on `state.json` and `ledger.jsonl` under
    /// `accounts.lock`.
    /// # Errors
    /// [`Store::load`]'s refusal, naming the file; `edit`'s own, with nothing written; or
    /// the file that could not be locked or written, by path.
    pub fn transact(
        &self,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire()
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        self.transact_held(&held, now, note, edit)
    }

    /// Runs [`Store::transact`] with a bounded lock wait over `ladder` ([`LOCK_HELD`]).
    /// # Errors
    /// As [`Store::transact`]; and exactly [`LOCK_HELD`] when the lock stayed held past the
    /// ladder: nothing was read, `edit` did not run, and the caller asks again later.
    pub fn transact_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_string())?;
        self.transact_held(&held, now, note, edit)
    }

    /// Saves an explicit selection before its login worker starts.
    /// # Errors
    /// Invalid label, unavailable entropy, or a bounded state transaction refusal.
    pub fn request_pin(&self, label: &str, now: i64) -> Result<PinRequest, String> {
        if let Some(why) = label_refusal(label) {
            return Err(why.to_owned());
        }
        let nonce =
            aterm_uds::rand::hex_token::<16>().map_err(|e| format!("account selection: {e}"))?;
        let request = PinRequest::new(label, nonce)
            .ok_or_else(|| "invalid account selection request".to_owned())?;
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            state.pending_pin = Some(request.clone());
            Ok(Vec::new())
        })?;
        Ok(request)
    }

    /// Pins an eligible account, or saves an explicit sign-in request for an unenrolled or
    /// signed-out account. Eligibility uses the current login bindings under `accounts.lock`.
    /// # Errors
    /// An unknown label, a sign-in target without an email, unavailable entropy, or a bounded
    /// state/login transaction refusal. A store error never becomes a sign-in request.
    pub fn select_account(
        &self,
        file_roster: &Roster,
        label: &str,
        now: i64,
    ) -> Result<PinSelection, String> {
        let mut selected = PinSelection::Pinned;
        self.transact_logins_within(&LOCK_LADDER, now, &mut None, |state, logins| {
            let roster = bound(file_roster, logins);
            let enrolled = enrolled(&roster);
            match pin_refusal(state, &roster, &enrolled, label) {
                None => pin(state, &roster, &enrolled, label, now).map(|event| vec![event]),
                Some(why @ PinRefusal::NotListed) => Err(why.message(label)),
                Some(PinRefusal::NotEnrolled | PinRefusal::SignedOut) => {
                    if roster
                        .row(label)
                        .and_then(|row| row.email.as_ref())
                        .is_none()
                    {
                        return Err(format!("`{label}` has no sign-in email"));
                    }
                    let nonce = aterm_uds::rand::hex_token::<16>()
                        .map_err(|e| format!("account selection: {e}"))?;
                    let request = PinRequest::new(label, nonce)
                        .ok_or_else(|| "invalid account selection request".to_owned())?;
                    state.pending_pin = Some(request.clone());
                    selected = PinSelection::Renew(request);
                    Ok(Vec::new())
                }
            }
        })?;
        Ok(selected)
    }

    /// Cancels this request after startup or login failure, preserving a newer choice.
    /// # Errors
    /// A bounded state transaction refusal.
    pub fn cancel_pin_request(&self, request: &PinRequest, now: i64) -> Result<(), String> {
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            if state.pending_pin.as_ref() == Some(request) {
                state.pending_pin = None;
            }
            Ok(Vec::new())
        })?;
        Ok(())
    }

    /// Runs an atomic transaction over both `state.json` and `logins.json` under `accounts.lock`
    /// ([`Store::transact`]).
    /// # Errors
    /// As [`Store::transact`], for either file.
    pub fn transact_logins(
        &self,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire()
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        self.transact_logins_held(&held, now, note, edit)
    }

    /// Runs [`Store::transact_logins`] with the bounded [`Store::transact_within`] lock wait.
    /// # Errors
    /// As [`Store::transact_within`].
    pub fn transact_logins_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_string())?;
        self.transact_logins_held(&held, now, note, edit)
    }

    /// The logins' transaction itself ([`Store::transact_held`]).
    pub(super) fn transact_logins_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let (mut logins, read) = self.load_logins(now).map_err(|why| {
            format!(
                "{why}: {} is left as it is and nothing was written",
                self.logins_file().display()
            )
        })?;
        let before = logins.clone();
        let mut saved = Ok(());
        let events = self.transact_held(held, now, note, |state| {
            let events = edit(state, &mut logins)?;
            if logins != before {
                saved = self.save_logins(&logins);
            }
            saved
                .as_ref()
                .map_err(|e| format!("{}: {e}", self.logins_file().display()))?;
            Ok(events)
        })?;
        if note.is_none() {
            *note = read;
        }
        Ok(events)
    }

    fn transact_held(
        &self,
        _held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let (mut state, read) = self.load(now).map_err(|why| {
            format!(
                "{why}: {} is left as it is and nothing was written (move it aside to start \
                 from an empty state)",
                self.state_file().display()
            )
        })?;
        *note = read;
        if let Ok(id) = self.machine_id(&state.machine) {
            state.machine = id;
        }
        let before = state.clone();
        let events = edit(&mut state)?;
        if state != before {
            drop_stale_sessions(&mut state, now);
            self.save(&state)
                .map_err(|e| format!("{}: {e}", self.state_file().display()))?;
        }
        self.append(&events)
            .map_err(|e| format!("{}: {e}", self.ledger_path().display()))?;
        if events.iter().any(Event::wakes) {
            self.touch_wake()
                .map_err(|e| format!("{}: {e}", self.wake_path().display()))?;
        }
        Ok(events)
    }
}

/// Outcome of a [`record_wall`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The session is on record on `account`.
    Wall {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        /// The window it is recorded under: the row's limit type, or [`UNNAMED_WINDOW`].
        window: String,
        /// The reset it names.
        until: Option<i64>,
        /// The ledger rows this call appended.
        events: usize,
    },
    /// No roster, or the process's login is no row's of it, nor one a renewal moved a row off
    /// ([`Me::label`]).
    NotListed,
    /// The process's home is not the default one, or it does not authenticate by a stored login.
    NotStored,
    /// The row is older than the process.
    NotThisProcess,
    /// The row was written before the owner cleared this account's walls ([`pin`]).
    Cleared,
    /// What a session's screen said of the account's usage is on record now ([`record_usage`]).
    Usage {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        events: usize,
    },
    /// Everything this call would write is on record.
    Unchanged,
    /// The state is not this build's to write, or a file could not be locked or written.
    Refused(String),
}

impl Recorded {
    /// Returns whether the recorder failed because `accounts.lock` remained held.
    #[must_use]
    pub fn lock_held(&self) -> bool {
        matches!(self, Recorded::Refused(why) if why == LOCK_HELD)
    }

    /// Single-word outcome code logged by the host.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Recorded::Wall { .. } => "wall",
            Recorded::Usage { .. } => "usage",
            Recorded::NotListed => "not-listed",
            Recorded::NotStored => "not-stored",
            Recorded::NotThisProcess => "not-this-process",
            Recorded::Cleared => "cleared",
            Recorded::Unchanged => "unchanged",
            Recorded::Refused(_) => "refused",
        }
    }
}

/// Records a standing usage wall hit by a supervised session under a bounded store lock.
pub fn record_wall(
    store: &Store,
    roster: &Roster,
    sid: &str,
    who: &ProcessLogin,
    wall: &super::upgrade::StandingWall,
    process_started: i64,
    now: i64,
) -> Recorded {
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.stored_login {
        return Recorded::NotStored;
    }
    let written = i64::try_from(wall.at).unwrap_or(i64::MAX);
    if written < process_started {
        return Recorded::NotThisProcess;
    }
    let at = written.min(now);
    let until = believed_reset(wall.until.and_then(|until| i64::try_from(until).ok()), at);
    let window = wall
        .window
        .as_deref()
        .map(|w| one_line(w, WORD_MAX))
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| UNNAMED_WINDOW.to_string());
    let sid = one_line(sid, WORD_MAX);
    let mut changed = false;
    let mut cleared = false;
    let mut note = None;
    let done = store.transact_within(store.ladder, now, &mut note, |state| {
        let before = state.clone();
        let on_it = state
            .sessions
            .get(&sid)
            .is_some_and(|row| row.account == account);
        let mut events = if on_it {
            Vec::new()
        } else {
            note_session(state, &sid, account, None, now)
        };
        cleared = state
            .accounts
            .get(account)
            .is_some_and(|record| record.cleared_since(&window, at, now).is_some());
        events.extend(note_wall(state, account, &window, until, at, now));
        note_credits_word(state, account, wall.credits.as_ref(), at);
        changed = *state != before;
        Ok(events
            .into_iter()
            .map(|event| match event.sid {
                Some(_) => event,
                None => event.with_sid(&sid),
            })
            .collect())
    });
    match done {
        Err(why) => Recorded::Refused(why),
        Ok(_) if cleared => Recorded::Cleared,
        Ok(events) if events.is_empty() && !changed => Recorded::Unchanged,
        Ok(events) => Recorded::Wall {
            account: account.to_string(),
            window,
            until,
            events: events.len(),
        },
    }
}

fn note_credits_word(
    state: &mut State,
    account: &str,
    word: Option<&super::upgrade::CreditsWord>,
    at: i64,
) {
    use super::upgrade::CreditsWord;
    let Some(word) = word else {
        return;
    };
    let Some(record) = account_mut(state, account) else {
        return;
    };
    let held = record.credits.as_ref();
    if held.is_some_and(|held| at < held.at())
        || (*word == CreditsWord::On && matches!(held, Some(Credits::InUse(_))))
    {
        return;
    }
    record.credits = Some(match word {
        CreditsWord::On => Credits::On(at),
        CreditsWord::Off { reason, until } => Credits::Off(CreditsOff {
            reason: reason
                .as_deref()
                .map(|r| one_line(r, WORD_MAX))
                .unwrap_or_default(),
            at,
            until: until.and_then(|u| i64::try_from(u).ok()),
        }),
    });
}

/// Screen observation that usage credits are carrying the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditsRead {
    /// The reset of the window they carry past.
    pub until: Option<i64>,
}

/// Screen observation of a Claude Code quota-window warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarningRead {
    /// The window: the week (`true`) or the five-hour session window.
    pub weekly: bool,
    /// Whole percent used.
    pub pct: u32,
    /// Its reset, placed by the caller.
    pub reset: Option<i64>,
}

/// Combined screen usage observations (`credits` and `warning`) passed to [`record_usage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSaid {
    /// When it was true ([`NOTICES_DRAWN_S`]).
    pub at: i64,
    /// The local day of [`Self::at`].
    pub day: i64,
    /// Credits carry the session.
    pub credits: Option<CreditsRead>,
    /// The warning that a window is filling.
    pub warning: Option<WarningRead>,
}

/// Records screen-observed usage credits and quota warnings for a session under a bounded store
/// lock.
pub fn record_usage(
    store: &Store,
    roster: &Roster,
    sid: &str,
    who: &ProcessLogin,
    said: &UsageSaid,
    now: i64,
) -> Recorded {
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.stored_login {
        return Recorded::NotStored;
    }
    let at = said.at.min(now);
    let sid = one_line(sid, WORD_MAX);
    let mut changed = false;
    let mut note = None;
    let done = store.transact_within(store.ladder, now, &mut note, |state| {
        let Some(record) = account_mut(state, account) else {
            return Ok(Vec::new());
        };
        let before = record.clone();
        let mut events = Vec::new();
        if let Some(credits) = said.credits {
            events.extend(note_in_use(record, account, credits, at, said.day, now));
        }
        if let Some(warning) = said.warning {
            events.extend(note_warning(record, account, warning, at, now));
        }
        changed = *record != before;
        Ok(events
            .into_iter()
            .map(|event| event.with_sid(&sid))
            .collect())
    });
    match done {
        Err(why) => Recorded::Refused(why),
        Ok(_) if !changed => Recorded::Unchanged,
        Ok(events) => Recorded::Usage {
            account: account.to_string(),
            events: events.len(),
        },
    }
}

fn note_in_use(
    record: &mut Account,
    account: &str,
    read: CreditsRead,
    at: i64,
    day: i64,
    now: i64,
) -> Option<Event> {
    match record.credits.as_mut() {
        Some(held) if at < held.at() => return None,
        Some(Credits::InUse(c)) if c.since <= at && c.stands(at) => {
            c.seen = c.seen.max(at);
            c.until = c.until.max(read.until);
            return None;
        }
        Some(held) if at <= held.at() => return None,
        _ => {}
    }
    let told = record.credits_day != Some(day);
    if told {
        record.credits_day = Some(day);
    }
    record.credits = Some(Credits::InUse(CreditsUse {
        since: at,
        seen: at,
        until: read.until,
        told,
    }));
    Some(Event::new(now, Kind::Credits, account).because("in use"))
}

fn note_warning(
    record: &mut Account,
    account: &str,
    warning: WarningRead,
    at: i64,
    now: i64,
) -> Option<Event> {
    let (slot, window) = if warning.weekly {
        (&mut record.warned_week, "weekly")
    } else {
        (&mut record.warned_session, "session")
    };
    if slot.as_ref().is_some_and(|held| at < held.at) {
        return None;
    }
    let pct = warning.pct.min(PCT_MAX);
    let moved = slot
        .as_ref()
        .is_none_or(|held| (held.pct, held.reset) != (Some(pct), warning.reset));
    *slot = Some(WindowSaid {
        pct: Some(pct),
        reset: warning.reset,
        at,
    });
    moved.then(|| Event::new(now, Kind::Warned, account).because(&format!("{window} {pct}%")))
}

/// Returns the labels in roster order whose active usage-credits episode warrants a digest entry.
#[must_use]
pub fn credits_noted(roster: &Roster, state: &State, now: i64) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            matches!(
                state.accounts.get(&row.label).and_then(|a| a.credits.as_ref()),
                Some(Credits::InUse(c)) if c.told && c.stands(now)
            )
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Returns `(label, reason)` pairs in roster order for accounts with a nearly full weekly window
/// and credits off.
#[must_use]
pub fn windows_full(roster: &Roster, state: &State, now: i64) -> Vec<(String, String)> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let account = state.accounts.get(&row.label)?;
            let Some(Credits::Off(off)) = &account.credits else {
                return None;
            };
            if off.until.is_some_and(|until| now >= until) {
                return None;
            }
            let (pct, end) = account.warned_week.as_ref()?.figure(WEEK_WINDOW_S, now)?;
            (pct >= WINDOW_FULL_PCT && end.saturating_sub(now) > WINDOW_FULL_AHEAD_S)
                .then(|| (row.label.clone(), off.reason.clone()))
        })
        .collect()
}

/// Removes ended session `sid` from the store under a bounded lock wait, and touches `wake` when
/// that released a canary hold. Returns whether the state held anything of `sid`.
/// # Errors
///
/// As [`Store::transact_within`]: the state is not this build's to write, a file could not
/// be locked or written, or the lock stayed held (exactly [`LOCK_HELD`]); or `wake` could not
/// be touched.
pub fn record_gone(store: &Store, sid: &str, now: i64) -> Result<bool, String> {
    if !store.state_file().exists() {
        return Ok(false);
    }
    let sid = one_line(sid, WORD_MAX);
    let mut forgot = false;
    let mut held = false;
    let mut note = None;
    store.transact_within(store.ladder, now, &mut note, |state| {
        let holding = state.canary_holds(now);
        forgot = forget_session(state, &sid);
        held = holding && !state.canary_holds(now);
        Ok(Vec::new())
    })?;
    if held {
        store
            .touch_wake()
            .map_err(|e| format!("{}: {e}", store.wake_path().display()))?;
    }
    Ok(forgot)
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "accounts_logins_tests.rs"]
mod logins_tests;
