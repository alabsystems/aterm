// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TOKEN STEP THAT A CHECK STOPPED (plan package FL-136;
//! `docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3 and S1): the capture of an account's
//! Claude Code token that ended on what another try alone does not pass
//! ([`MetWord::holds_a_token_step`]: a check or a block page, the consent page's human check, a
//! consent page of another account, or one on the account's headless profile that did not show
//! the account (FL-137), a closed window, no desktop, a code page nobody typed a code on where
//! the window read no mailbox, the window's deadline) leaves one record in [`STEPS_FILE`], and
//! the step is held with no clock. Nothing tries it again until something changes
//! ([`Stopped::holds`]): a new Claude Code or aterm build, the account's window store, a mailbox
//! configuration or its saved Google mail grant written after the stop, or a token made after
//! the stop. The account's next sign-in, in aterm's window or on its headless profile, and a
//! person's `aterm harness accounts token <label>` run the step whatever the record says: a stop
//! that holds replaces the record, a token made (which passed the page) clears it, and any other
//! end (a person's Ctrl-C, a browser that did not start, a page that did not load) leaves it as it
//! was, since it says nothing of the check that stood.
//!
//! A record is the stop's closed word, when it was met, and the Claude Code and aterm builds that
//! met it: never a reason line, a host, an address, a code or a token byte. No earlier build reads
//! this file, so no reader that refuses a word it does not know meets one here, as one may in
//! `tokens.json` or `logins.json`; a later build's record this one does not read is kept as it
//! is ([`put`]), and a file that does not read is never written over.

use super::accounts::{self, Store};
use super::accounts_tokens::{TABLE_LOCK, date};
use super::open_url::MetWord;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The records' file in Claude Code's accounts store, beside `tokens.json` and under its lock
/// ([`TABLE_LOCK`]).
pub const STEPS_FILE: &str = "tokens-steps.json";

/// The file's schema.
const SCHEMA: i64 = 1;

/// The most of [`STEPS_FILE`] a read takes.
const MAX_STEPS: usize = 1 << 16;

/// The longest build version a record keeps.
const MAX_BUILD: usize = 64;

/// One account's stopped token step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    /// What stopped it, as its closed word; one that holds a step.
    pub word: MetWord,
    /// When, Unix seconds: the stop's own second, rounded up.
    pub at: u64,
    /// The Claude Code build that ran the capture; empty where it was not known.
    pub cli: String,
    /// The aterm build that ran it ([`aterm_stamp::build_id`]); empty where it was not known.
    pub aterm: String,
}

/// What a held step is judged against: each fact as it stands now, `None` where it is not known.
#[derive(Clone, Copy, Debug, Default)]
pub struct Since<'a> {
    /// The Claude Code build a capture would run now.
    pub cli: Option<&'a str>,
    /// This aterm's build.
    pub aterm: Option<&'a str>,
    /// When the account's window store, a mailbox configuration or the account's saved Google
    /// mail grant was last written ([`route_at`]).
    pub route_at: Option<u64>,
    /// When the token the vault holds for the account was made.
    pub token_at: Option<u64>,
}

impl Stopped {
    /// A record of `word` met at `at` by the builds `cli` and `aterm`; `None` where `word`
    /// holds no step, or a build's version is not one a record keeps (an empty one is unknown).
    #[must_use]
    pub fn new(word: MetWord, at: u64, cli: &str, aterm: &str) -> Option<Self> {
        (word.holds_a_token_step() && is_build(cli) && is_build(aterm)).then(|| Self {
            word,
            at,
            cli: cli.to_owned(),
            aterm: aterm.to_owned(),
        })
    }

    /// Whether the step stays held: nothing in `since` changed after the stop. A build counts as
    /// changed only where both its versions are known and differ; a window store, mailbox
    /// configuration or Google mail grant written after the stop, or a token made after it, ends
    /// the hold too. No time by itself ends it: there is no clock here.
    #[must_use]
    pub fn holds(&self, since: &Since<'_>) -> bool {
        let moved = |then: &str, now: Option<&str>| {
            now.is_some_and(|now| !then.is_empty() && !now.is_empty() && then != now)
        };
        let after = |t: Option<u64>| t.is_some_and(|t| t > self.at);
        !(moved(&self.cli, since.cli)
            || moved(&self.aterm, since.aterm)
            || after(since.route_at)
            || after(since.token_at))
    }

    /// The `token-stopped` note's reason word (one of `journal::WHYS`). Every word is named,
    /// so a word added to [`MetWord`] is given its own here before anything builds.
    #[must_use]
    pub const fn why(&self) -> &'static str {
        match self.word {
            MetWord::WindowHumanCheck | MetWord::HumanCheck => "human-check",
            MetWord::ConsentUnproven => "consent-unproven",
            MetWord::WindowBotCheck | MetWord::BotCheck => "bot-check",
            MetWord::WindowBlockPage | MetWord::BlockPage => "block-page",
            MetWord::WindowWrongAccount | MetWord::ConsentWrongAccount => "wrong-account",
            MetWord::WindowClosed => "window-closed",
            MetWord::WindowNoDesktop => "no-desktop",
            MetWord::WindowCodeNoMailbox => "code-no-mailbox",
            MetWord::WindowDeadline => "deadline",
            // Words that hold no step, of which no record is made ([`Self::new`]).
            MetWord::BrowserStart
            | MetWord::BrowserControl
            | MetWord::BrowserSignIn
            | MetWord::Stall
            | MetWord::NotAuthorized => "failed",
        }
    }

    /// What stopped it, in words with no host or reason. Every word is named, as in
    /// [`Self::why`].
    #[must_use]
    pub fn what(&self) -> String {
        match self.word {
            MetWord::WindowHumanCheck => {
                "the consent page held Authorize behind a human check".to_owned()
            }
            MetWord::HumanCheck => "the consent page on the account's headless browser profile \
                                    carried a human check"
                .to_owned(),
            MetWord::ConsentUnproven => {
                "the consent page on the account's headless browser profile did not show the \
                 account as the one signed in"
                    .to_owned()
            }
            MetWord::ConsentWrongAccount => {
                "the consent page on the account's headless browser profile named another \
                 account"
                    .to_owned()
            }
            MetWord::WindowBotCheck | MetWord::BotCheck => {
                "Cloudflare's check did not clear".to_owned()
            }
            MetWord::WindowBlockPage | MetWord::BlockPage => {
                "Cloudflare's block page stood before the consent page".to_owned()
            }
            MetWord::WindowWrongAccount => "the consent page named another account".to_owned(),
            MetWord::WindowClosed => "aterm's sign-in window was closed".to_owned(),
            MetWord::WindowNoDesktop => {
                "this Mac had no desktop to open aterm's sign-in window on".to_owned()
            }
            MetWord::WindowCodeNoMailbox => {
                "the code page waited for a login code nobody typed".to_owned()
            }
            MetWord::WindowDeadline => "aterm's sign-in window did not finish in time".to_owned(),
            // Words that hold no step, of which no record is made ([`Self::new`]).
            word @ (MetWord::BrowserStart
            | MetWord::BrowserControl
            | MetWord::BrowserSignIn
            | MetWord::Stall
            | MetWord::NotAuthorized) => word.words(),
        }
    }

    /// The record as a surface shows it on demand, `step` being the person's own run where the
    /// account's row has a label: when, what stopped it, and what tries it again.
    #[must_use]
    pub fn words(&self, step: Option<&str>) -> String {
        let person = step.map_or_else(String::new, |step| format!(", or by `{step}`"));
        format!(
            "stopped {}: {}; it is tried again only at the account's next sign-in in aterm's \
             sign-in window or on its headless browser profile, with a new Claude Code or aterm \
             build{person}",
            date(self.at),
            self.what()
        )
    }

    /// The record as a report's JSON carries it: its word and when.
    #[must_use]
    pub fn json(&self) -> aterm_json::Value {
        aterm_json::json!({"word": self.word.as_str(), "at": self.at})
    }

    fn to_json(&self, key: &str) -> aterm_json::Value {
        aterm_json::json!({
            "key": key,
            "word": self.word.as_str(),
            "at": self.at,
            "cli": self.cli,
            "aterm": self.aterm,
        })
    }

    fn of_json(value: &aterm_json::Value) -> Option<(String, Self)> {
        let text = |name: &str| value.get(name).and_then(aterm_json::Value::as_str);
        let key = text("key").filter(|key| super::journal::Key::new(key).is_some())?;
        let word = MetWord::parse(text("word")?)?;
        let at = value.get("at").and_then(aterm_json::Value::as_u64)?;
        let step = Self::new(word, at, text("cli")?, text("aterm")?)?;
        Some((key.to_owned(), step))
    }
}

/// Whether `version` is one a record keeps: empty, or at most [`MAX_BUILD`] bytes of
/// `[0-9A-Za-z.+_-]`.
fn is_build(version: &str) -> bool {
    version.len() <= MAX_BUILD
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
}

/// Every account's stopped step on this Mac, by account key: none where there is no file yet.
/// A record that does not read (a later build's word, a key no record holds) is left out.
///
/// # Errors
/// The file is there and does not read, is not private, or is not a steps file of schema 1.
pub fn read(store: &Store) -> io::Result<BTreeMap<String, Stopped>> {
    Ok(rows(store)?.iter().filter_map(Stopped::of_json).collect())
}

/// The file's rows as it holds them, each whether it reads as a record or not: none where there
/// is no file yet.
///
/// # Errors
/// As [`read`]'s.
fn rows(store: &Store) -> io::Result<Vec<aterm_json::Value>> {
    let dir = store.shared_dir();
    let bytes = match aterm_uds::private_file::read_private(&dir, STEPS_FILE, MAX_STEPS) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(Vec::new()),
        Err(why) => return Err(io::Error::other(format!("{STEPS_FILE}: {why:?}"))),
    };
    let not = || io::Error::other(format!("{STEPS_FILE} is not a steps file of schema 1"));
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).map_err(|_| not())?;
    if value.get("schema").and_then(aterm_json::Value::as_i64) != Some(SCHEMA) {
        return Err(not());
    }
    value
        .get("steps")
        .and_then(aterm_json::Value::as_array)
        .cloned()
        .ok_or_else(not)
}

/// Sets the account `key`'s record to `step`, or clears it (`None`), under `tokens.lock`
/// (waiting for it as `accounts.lock`'s takers do), writing the file whole, atomically, mode
/// 0600; nothing is written where the record already says it. Every other account's row is
/// kept as the file holds it, a row this build does not read included (a later build's word),
/// so no account's hold is dropped by another's write.
///
/// # Errors
/// The lock stays held; the file is there and does not read, and so is left as it is (every
/// step stays held while it does not read: `accounts_tokens::step_due`); or it cannot be
/// written.
pub fn put(store: &Store, key: &str, step: Option<&Stopped>) -> io::Result<()> {
    let dir = store.shared_dir();
    store.ensure()?;
    let _lock = accounts::lock_at_within(&dir.join(TABLE_LOCK), &accounts::LOCK_LADDER)?
        .ok_or_else(|| io::Error::other(format!("{TABLE_LOCK} stayed held")))?;
    let rows = rows(store)?;
    let mine =
        |row: &aterm_json::Value| row.get("key").and_then(aterm_json::Value::as_str) == Some(key);
    let held: Vec<&aterm_json::Value> = rows.iter().filter(|row| mine(row)).collect();
    let says = match held.as_slice() {
        [] => step.is_none(),
        [row] => Stopped::of_json(row).is_some_and(|(_, now)| Some(&now) == step),
        _ => false,
    };
    if says {
        return Ok(());
    }
    let mut kept: Vec<aterm_json::Value> = rows.iter().filter(|row| !mine(row)).cloned().collect();
    if let Some(step) = step {
        kept.push(step.to_json(key));
    }
    let text = aterm_json::to_string(&aterm_json::json!({"schema": SCHEMA, "steps": kept}))
        .map_err(|why| io::Error::other(why.to_string()))?;
    aterm_uds::private_file::write_private(&dir, STEPS_FILE, text.as_bytes())
}

/// When what lets a held step through without a person was last written, Unix seconds: the
/// account `email`'s window store's record that a sign-in in aterm's window signed it in, the
/// mailbox configurations the window reads a login code through (the store's, `~/.secrets`'
/// two and `~/.config/aterm`'s, `home` being the owner's), and the account's saved Google mail
/// grant there, which lets the window read a Gmail row's code with no password
/// (`MailboxConfig::window_reads`). `None` where none is there.
#[must_use]
pub fn route_at(store: &Store, home: Option<&Path>, email: &str) -> Option<u64> {
    use super::headless_oauth::{MAILBOX_TOML_FILE, SECRETS_MAILBOX_TOML_FILE};
    let mut paths: Vec<PathBuf> = vec![store.dir.join(MAILBOX_TOML_FILE)];
    #[cfg(target_os = "macos")]
    paths.push(super::signin_window::store_marker(&store.dir, email));
    if let Some(home) = home {
        let secrets = home.join(".secrets");
        paths.push(secrets.join(SECRETS_MAILBOX_TOML_FILE));
        paths.push(secrets.join(MAILBOX_TOML_FILE));
        paths.push(home.join(".config/aterm").join(MAILBOX_TOML_FILE));
        paths.extend(aterm_mail::oauth::session::grant_file(home, email));
    }
    paths
        .iter()
        .filter_map(|path| {
            let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
            Some(
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs(),
            )
        })
        .max()
}

#[cfg(test)]
#[path = "token_steps_tests.rs"]
mod tests;
