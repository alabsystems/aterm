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
//! configuration file or its saved Google mail grant written after the stop, a token made after
//! the stop, or, for a code page nobody typed a code on, aterm's window reading the account's
//! mailbox now (the vault's mailbox entries included, which carry no time to date them by). The
//! account's next sign-in, in aterm's window or on its headless profile, and a
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
use super::accounts_tokens::{Gap, TABLE_LOCK, date};
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
    /// When the account's window store, a mailbox configuration file or the account's saved
    /// Google mail grant was last written ([`route_at`]).
    pub route_at: Option<u64>,
    /// When the token the vault holds for the account was made.
    pub token_at: Option<u64>,
    /// Whether aterm's sign-in window reads the account's login code on this Mac now, from any
    /// mailbox configuration ([`mailbox_read`]); asked only of a step that waits on a mailbox
    /// ([`Stopped::waits_on_a_mailbox`]), and never read by [`Tried::holds`].
    pub mailbox_read: Option<bool>,
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
    /// configuration file or Google mail grant written after the stop, or a token made after it,
    /// ends the hold too, and so does aterm's window reading the account's mailbox now where the
    /// stop was a code page nobody typed a code on, which only a mailbox the window reads lets
    /// through. No time by itself ends it: there is no clock here.
    #[must_use]
    pub fn holds(&self, since: &Since<'_>) -> bool {
        let moved = |then: &str, now: Option<&str>| {
            now.is_some_and(|now| !then.is_empty() && !now.is_empty() && then != now)
        };
        !(moved(&self.cli, since.cli)
            || moved(&self.aterm, since.aterm)
            || written_after(self.at, since)
            || (self.waits_on_a_mailbox() && since.mailbox_read == Some(true)))
    }

    /// Whether what stopped it was a mailbox missing: the code page in aterm's window asked for a
    /// login code, and the window read no mailbox of the account ([`MetWord::WindowCodeNoMailbox`]).
    /// A mailbox the window reads now lets it through ([`Since::mailbox_read`]), whenever and
    /// wherever that mailbox was configured.
    #[must_use]
    pub fn waits_on_a_mailbox(&self) -> bool {
        self.word == MetWord::WindowCodeNoMailbox
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

/// `version` as a [`Tried`] record keeps it: its bytes of `[0-9A-Za-z.+_-]`, the first
/// [`MAX_BUILD`] of them, so the record reads back as it was written ([`is_build`]).
fn build_word(version: &str) -> String {
    version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'))
        .take(MAX_BUILD)
        .collect()
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
/// step stays held while it does not read: `accounts_tokens::backfill_due`); or it cannot be
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

/// Whether `since` names a window store, mailbox configuration file or Google mail grant
/// written, or a token made, after the second `at`. A vault mailbox entry is no file and dates
/// nothing ([`route_at`]).
fn written_after(at: u64, since: &Since<'_>) -> bool {
    let after = |t: Option<u64>| t.is_some_and(|t| t > at);
    after(since.route_at) || after(since.token_at)
}

// THE GAPS THIS MAC'S UPKEEP RAN A CAPTURE FOR (plan package FL-138,
// `accounts_tokens::backfill_run`): once per gap, on record. The upkeep runs a capture for each gap
// it finds of an account's token (the gap, and the `minted_at` of the token the vault held) once: a
// capture that made no token, whatever it ended in, leaves one record here, kept past a restart,
// and the gap is not run again until something changes that may let the next capture pass
// ([`Tried::holds`]: a new Claude Code or aterm build, the account's window store, a mailbox
// configuration file or its saved Google mail grant written after it, a token made after it). A
// vault mailbox entry does not let it through, though it lets a code page's stop through: the
// entries date nothing, and the run that met that code page left this record as surely as the stop.
// It holds what the stop records above do not: a mailbox that failed after the window asked for a
// code, a consent page that named nobody, a headless capture that ran out, a browser that did not
// start. The record is written before the capture starts, under the account's lock, and rewritten
// as it ended: a run whose record cannot be written opens no page. A capture that made the token
// clears the account's record. The record keys by account, never by route, so a row whose route
// changed runs no gap twice. Its file is beside [`STEPS_FILE`], under the same lock, and holds the
// gap's closed word, its token's `minted_at`, when, and the builds that ran it: never a reason
// line, a host, an address, a code or a token byte.

/// The upkeep's record of the gaps it ran a capture for, beside [`STEPS_FILE`] and under its
/// lock ([`TABLE_LOCK`]).
pub const TRIED_FILE: &str = "tokens-tried.json";

/// The gap of one account's token this Mac's upkeep last ran a capture for, which made no token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tried {
    /// The gap.
    pub gap: Gap,
    /// The `minted_at` of the token the vault held then; `None`: it held none.
    pub minted_at: Option<u64>,
    /// When the capture ended, Unix seconds, rounded up.
    pub at: u64,
    /// The Claude Code build it ran; empty where none was known.
    pub cli: String,
    /// The aterm build that ran it ([`aterm_stamp::build_id`]); empty where it was not known.
    pub aterm: String,
}

impl Tried {
    /// A record of the gap `gap` of the token made at `minted_at`, whose capture ended at `at`,
    /// run by the builds `cli` and `aterm`, each kept as [`build_word`] keeps it: every run that
    /// made no token leaves one, so no version a build prints lets its gap run again.
    #[must_use]
    pub fn new((gap, minted_at): (Gap, Option<u64>), at: u64, cli: &str, aterm: &str) -> Self {
        Self {
            gap,
            minted_at,
            at,
            cli: build_word(cli),
            aterm: build_word(aterm),
        }
    }

    /// Whether the gap stays held: nothing in `since` changed after the capture. A build counts
    /// as changed where its version now is known and is not the one that ran it (a Claude Code
    /// build where none was known counts); a window store, mailbox configuration file or Google
    /// mail grant written after it, or a token made after it, ends the hold too. A mailbox the
    /// window reads now does not ([`Since::mailbox_read`] is not read here): a vault mailbox
    /// entry dates nothing, and the run this record names may have ended with it as it stands
    /// now. No time by itself ends it.
    #[must_use]
    pub fn holds(&self, since: &Since<'_>) -> bool {
        let moved = |then: &str, now: Option<&str>| {
            now.map(build_word)
                .is_some_and(|now| !now.is_empty() && then != now)
        };
        !(moved(&self.cli, since.cli)
            || moved(&self.aterm, since.aterm)
            || written_after(self.at, since))
    }

    /// The record as a surface shows it on demand, `step` being the person's own run where the
    /// account's row has a label: when, the gap, and what lets the upkeep run it again.
    #[must_use]
    pub fn words(&self, step: Option<&str>) -> String {
        let person = step.map_or_else(String::new, |step| format!(", such as `{step}` makes"));
        format!(
            "ran {} for its year-long token ({}): this Mac's upkeep's capture with nobody there \
             made no token, and it runs none again for that token until a new Claude Code or \
             aterm build, the account's window store, a mailbox configuration file or its saved \
             Google mail grant written after it, or a token made after it{person}",
            date(self.at),
            self.gap.word()
        )
    }

    /// The record as a report's JSON carries it: its gap's word, its token's `minted_at` and
    /// when.
    #[must_use]
    pub fn json(&self) -> aterm_json::Value {
        aterm_json::json!({"gap": self.gap.as_str(), "minted_at": self.minted_at, "at": self.at})
    }

    fn to_json(&self, key: &str) -> aterm_json::Value {
        aterm_json::json!({
            "key": key,
            "gap": self.gap.as_str(),
            "minted_at": self.minted_at,
            "at": self.at,
            "cli": self.cli,
            "aterm": self.aterm,
        })
    }

    fn of_json(value: &aterm_json::Value) -> Option<(String, Self)> {
        let text = |name: &str| value.get(name).and_then(aterm_json::Value::as_str);
        let key = text("key").filter(|key| super::journal::Key::new(key).is_some())?;
        let gap = Gap::parse(text("gap")?)?;
        let minted_at = match value.get("minted_at")? {
            aterm_json::Value::Null => None,
            at => Some(at.as_u64()?),
        };
        let at = value.get("at").and_then(aterm_json::Value::as_u64)?;
        let (cli, aterm) = (text("cli")?, text("aterm")?);
        if !is_build(cli) || !is_build(aterm) {
            return None;
        }
        Some((key.to_owned(), Self::new((gap, minted_at), at, cli, aterm)))
    }
}

/// Every account's [`Tried`] record on this Mac, by account key: none where there is no file yet.
/// A record that does not read (a later build's word) is left out.
///
/// # Errors
/// The file is there and does not read, is not private, or is not a record file of schema 1.
pub fn read_tried(store: &Store) -> io::Result<BTreeMap<String, Tried>> {
    Ok(tried_rows(store)?
        .iter()
        .filter_map(Tried::of_json)
        .collect())
}

/// [`TRIED_FILE`]'s rows as it holds them: none where there is no file yet.
///
/// # Errors
/// As [`read_tried`]'s.
fn tried_rows(store: &Store) -> io::Result<Vec<aterm_json::Value>> {
    let dir = store.shared_dir();
    let bytes = match aterm_uds::private_file::read_private(&dir, TRIED_FILE, MAX_STEPS) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(Vec::new()),
        Err(why) => return Err(io::Error::other(format!("{TRIED_FILE}: {why:?}"))),
    };
    let not = || io::Error::other(format!("{TRIED_FILE} is not a record file of schema 1"));
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).map_err(|_| not())?;
    if value.get("schema").and_then(aterm_json::Value::as_i64) != Some(SCHEMA) {
        return Err(not());
    }
    value
        .get("tried")
        .and_then(aterm_json::Value::as_array)
        .cloned()
        .ok_or_else(not)
}

/// Sets the account `key`'s [`Tried`] record to `tried`, or clears it (`None`), under
/// `tokens.lock`, as [`put`] sets a stop's: the file whole, atomically, mode 0600; nothing is
/// written where the record already says it, and every other account's row is kept as the file
/// holds it.
///
/// # Errors
/// As [`put`]'s: the lock stays held, the file is there and does not read (every gap stays held
/// while it does not read), or it cannot be written.
pub fn put_tried(store: &Store, key: &str, tried: Option<&Tried>) -> io::Result<()> {
    let dir = store.shared_dir();
    store.ensure()?;
    let _lock = accounts::lock_at_within(&dir.join(TABLE_LOCK), &accounts::LOCK_LADDER)?
        .ok_or_else(|| io::Error::other(format!("{TABLE_LOCK} stayed held")))?;
    let rows = tried_rows(store)?;
    let mine =
        |row: &aterm_json::Value| row.get("key").and_then(aterm_json::Value::as_str) == Some(key);
    let held: Vec<&aterm_json::Value> = rows.iter().filter(|row| mine(row)).collect();
    let says = match held.as_slice() {
        [] => tried.is_none(),
        [row] => Tried::of_json(row).is_some_and(|(_, now)| Some(&now) == tried),
        _ => false,
    };
    if says {
        return Ok(());
    }
    let mut kept: Vec<aterm_json::Value> = rows.iter().filter(|row| !mine(row)).cloned().collect();
    if let Some(tried) = tried {
        kept.push(tried.to_json(key));
    }
    let text = aterm_json::to_string(&aterm_json::json!({"schema": SCHEMA, "tried": kept}))
        .map_err(|why| io::Error::other(why.to_string()))?;
    aterm_uds::private_file::write_private(&dir, TRIED_FILE, text.as_bytes())
}

/// When what lets a held step through without a person was last written, Unix seconds: the
/// account `email`'s window store's record that a sign-in in aterm's window signed it in, the
/// mailbox configuration files the window reads a login code through (the store's and
/// `~/.config/aterm`'s, `home` being the owner's), and the account's saved Google mail grant
/// there, which lets the window read a Gmail row's code with no password
/// (`MailboxConfig::window_reads`). `None` where none is there. The vault's two mailbox entries
/// are no files and carry no time of their own (the vault's `state.toml` is rewritten about
/// once a minute, each time this Mac hears the vault, whatever changed), so they date nothing
/// here: what they let through is judged by whether the window reads the account's mailbox now
/// ([`mailbox_read`]).
#[must_use]
pub fn route_at(store: &Store, home: Option<&Path>, email: &str) -> Option<u64> {
    use super::headless_oauth::MAILBOX_TOML_FILE;
    let mut paths: Vec<PathBuf> = vec![store.dir.join(MAILBOX_TOML_FILE)];
    #[cfg(target_os = "macos")]
    paths.push(super::signin_window::store_marker(&store.dir, email));
    if let Some(home) = home {
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

/// Where a mailbox read looks: the home it reads under, and the environment variable reader.
pub type EnvRead<'a> = (
    Option<&'a Path>,
    &'a dyn Fn(&str) -> Option<std::ffi::OsString>,
);

/// Whether aterm's sign-in window reads the login code of the account `email` (the row `label`)
/// on this Mac now, as its own desk would answer it
/// ([`super::accounts_sync::window_reads_mailbox_for_row_with`]): every mailbox configuration it
/// reads, the vault's two mailbox entries among them, the environment read through `var` alone.
#[must_use]
pub fn mailbox_read(
    store: &Store,
    (home, var): EnvRead<'_>,
    label: Option<&str>,
    email: &str,
) -> bool {
    super::accounts_sync::window_reads_mailbox_for_row_with(var, home, store, label, Some(email))
}

#[cfg(test)]
#[path = "token_steps_tests.rs"]
mod tests;
