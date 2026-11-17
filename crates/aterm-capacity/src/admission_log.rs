// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADMISSION LOG (`docs/DESIGN-landing-economy.md` slice 2, its sub-slice 2a): what this
//! account's admissions waited for and what the work they admitted then took, one JSON object a
//! line (`{"v":1,"kind":…,"at_ms":…}` and the fields its kind carries, [`Row`]), in
//! `<account state>/admission-log/` ([`default_dir`]), the account's own folder (`0700`). The
//! account's state folder is the one under the home the account database names, whatever `$HOME`
//! says, as the account's ledger's folder is ([`crate::ledger::user_dir`]): a tool run under a
//! scratch home (a release's test environment, a lab world) logs its real claims where every
//! session of the account reads them. A private state root (`$ATERM_STATE_HOME`, a test binary's
//! scratch root) keeps the log as it keeps every other state file. It is the rotating log of
//! `aterm_telemetry` ([`aterm_telemetry::Log`]): four files of 4 MiB, each opening with the host's
//! facts, losses said in gap lines. It is a folder of its own, not the telemetry log, so its rows
//! crowd out none of that log's.
//!
//! Its rows, and who writes them:
//! - `look` ([`crate::Waiter`]'s looks, in this crate): a look whose shortfall holds memory, ALONE
//!   for another claim, ALONE asked, or the kernel's pressure ([`Short::Memory`],
//!   [`Short::Alone`], [`Short::WantsAlone`], [`Short::Pressure`]): the waiter's claim, what the
//!   look read available, each claim its shortfall names with what it claims and still reserves
//!   (`holders`), and every held claim of memory the look was held to, whatever its shortfall
//!   names (`held_mem`). One at the first such look of a wait, then at most one a minute
//!   ([`LOOK_EVERY`]), and one at the look that admits a wait that wrote one. A wait is what
//!   waits, not the waiter value that looks: a need's words and memory in one ledger, in one
//!   process (`Waits`), so a caller that enters a fresh waiter at each look (a compiler unit's,
//!   a test binary's) still writes one row a minute, all under one id: the claim whose look wrote
//!   the wait's first row (`id`), with the claim of each later look where it is another
//!   (`claim`).
//! - `admitted` (an admission that owes a telemetry admission row, [`crate::telemetry`]:
//!   [`crate::admit_with`] and its kin, and every caller that gives an [`crate::telemetry::Asked`]
//!   or [`crate::telemetry::Owed`] its decision): the claim, when it asked and when it was
//!   admitted, every kind of shortfall its looks found, whether `--admit now` forced it, and its
//!   grant. An admission whose caller drives its waiter itself and owes no such row (verify's
//!   test binaries, a compiler unit's) writes none; a memory wait of one closes at its admitting
//!   `look` row.
//! - The rows verify writes through [`user`] (`stage`, `tests`, `run`): what each stage's claim
//!   was sized by and what the stage then took, joined to the `look` rows by the claim's id.
//!
//! The same rules as the admission row's ([`crate::telemetry`]): only this user's own ledger
//! ([`Ledger::user`]'s folder) writes `look` and `admitted` rows, so a scratch ledger (every
//! test's) writes none (`writer_in`); a write waits for no other writer's lock
//! (`waiting(Duration::ZERO)`) and is made after the look let the ledger's lock go; a row that
//! misses the lock or fails is lost and counted in a loss record in the folder
//! ([`aterm_telemetry::log::mark_lost`]), which the next append of any writer says in a gap line;
//! and a write never fails, delays or changes an admission. A caller that polls under a lock of
//! its own (verify's test dispatch) holds it across the write of a row it makes due: at most one
//! a minute for each wait. A test reads the rows written on its thread through [`capture`] (test
//! and debug builds), none of them to this account's folder.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use aterm_json::{Map, Value};

use crate::{Claim, Error, Ledger, Polled, Record, Short, Shortfall};

/// The log's folder under the account's state directory.
pub const DIR: &str = "admission-log";

/// The rows' schema, each row's `v`.
pub const SCHEMA: u64 = 1;

/// The least time between two `look` rows of one wait, ms: one at its first memory look, then at
/// most one a minute.
pub const LOOK_EVERY: u64 = 60_000;

/// How long a wait no look of this process has seen stays open in `Waits`, ms: two of
/// [`LOOK_EVERY`], far past any poller's gap between looks (250 ms to 2 s), so one whose waiter
/// was dropped unadmitted (an interrupt, a run that ended) is over by then.
pub const SILENT_ENDS: u64 = 2 * LOOK_EVERY;

/// The most waits [`Waits`] keeps open at once; past it, the one silent longest goes.
const WAITS_KEPT: usize = 256;

/// The log's folder under the aterm state directory `state`.
#[must_use]
pub fn dir_in(state: &Path) -> PathBuf {
    state.join(DIR)
}

/// The log's folder of the account whose ledger this process claims in ([`Ledger::user`]): under
/// a private state root where this process runs under one (`$ATERM_STATE_HOME`, a development
/// seam, or a test binary's scratch root: [`aterm_types::dirs::runs_under_private_state_root`]),
/// else under the state directory of the home the account database names
/// ([`account_dir`]), whatever `$HOME` says; elsewhere than Unix, the account's state directory
/// ([`aterm_types::dirs::account_state_dir`]). `None` where there is none.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if aterm_types::dirs::runs_under_private_state_root() {
            return aterm_types::dirs::state_dir().map(|state| dir_in(&state));
        }
        account_dir(aterm_uds::account::home())
    }
    #[cfg(not(unix))]
    {
        aterm_types::dirs::account_state_dir().map(|state| dir_in(&state))
    }
}

/// The log's folder of the account whose home is `home` (the account database's): its state
/// directory's ([`aterm_types::dirs::resolve_state_dir`] of that home and nothing else, so no
/// `$HOME` or `$XDG_STATE_HOME` moves it), as the account's ledger follows that home
/// ([`crate::ledger::user_dir`]). `None` with no home.
#[must_use]
pub fn account_dir(home: Option<PathBuf>) -> Option<PathBuf> {
    aterm_types::dirs::resolve_state_dir(
        None,
        aterm_types::dirs::StatePlatform {
            home,
            ..aterm_types::dirs::StatePlatform::default()
        },
    )
    .map(|state| dir_in(&state))
}

/// ONE ROW OF THE LOG: a JSON object holding its schema (`v`), its kind and when it was written
/// (`at_ms`, unix ms), and the fields its kind carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Row(Map);

impl Row {
    /// A row of `kind` written at `at_ms`.
    #[must_use]
    pub fn new(kind: &str, at_ms: u64) -> Row {
        let mut fields = Map::new();
        fields.insert("v".to_owned(), Value::from(SCHEMA));
        fields.insert("kind".to_owned(), Value::from(kind));
        fields.insert("at_ms".to_owned(), Value::from(at_ms));
        Row(fields)
    }

    /// A row of `kind` written now.
    #[must_use]
    pub fn now(kind: &str) -> Row {
        Row::new(kind, aterm_telemetry::now_ms())
    }

    /// This row with `key` set to `value`. The schema, kind and time are the row's own and are
    /// never replaced.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Row {
        if !matches!(key, "v" | "kind" | "at_ms") {
            self.0.insert(key.to_owned(), value.into());
        }
        self
    }

    /// Its kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.0.get("kind").and_then(Value::as_str).unwrap_or("")
    }

    /// When it was written, unix ms.
    #[must_use]
    pub fn at_ms(&self) -> u64 {
        self.0.get("at_ms").and_then(Value::as_u64).unwrap_or(0)
    }

    /// Its field `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Its field `key` as a number, where it is one that fits `u64`.
    #[must_use]
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(Value::as_u64)
    }

    /// Its field `key` as text, where it is text.
    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Its field `key` as a boolean, `false` where it is none.
    #[must_use]
    pub fn flag(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Every field.
    #[must_use]
    pub const fn fields(&self) -> &Map {
        &self.0
    }

    /// Its line in the log: the object and a line feed.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = aterm_json::to_string(&self.0).unwrap_or_default();
        line.push('\n');
        line
    }

    /// The row a line of the log holds: `None` for a torn line, another schema, or an object
    /// without a kind and a time.
    #[must_use]
    pub fn parse(line: &str) -> Option<Row> {
        let Value::Object(fields) = aterm_json::from_str::<Value>(line.trim_end()).ok()? else {
            return None;
        };
        let row = Row(fields);
        (row.u64("v") == Some(SCHEMA)
            && !row.kind().is_empty()
            && row.get("at_ms").and_then(Value::as_u64).is_some())
        .then_some(row)
    }
}

/// The rows this process could not write and left no line of: counted in each folder's loss
/// record, or where none could be left, said before this process's next row.
static LOST: aterm_telemetry::direct::Lost = aterm_telemetry::direct::Lost::new();

/// How many rows this process lost ([`Writer::write`]).
#[must_use]
pub fn lost() -> u64 {
    LOST.lost()
}

/// A WRITER OF THE LOG: rows go to its folder's log, each a write that waits for no lock.
#[derive(Clone, Debug)]
pub struct Writer {
    to: To,
}

#[derive(Clone, Debug)]
enum To {
    /// The log in this folder.
    Log(PathBuf),
    /// This thread's [`capture`].
    #[cfg(any(test, debug_assertions))]
    Capture,
}

impl Writer {
    /// The writer of the log in `dir`.
    #[must_use]
    pub const fn at(dir: PathBuf) -> Writer {
        Writer { to: To::Log(dir) }
    }

    /// Append `row`, waiting for no other writer's lock: whether it was written. One that was not
    /// is lost and counted ([`lost`]); nothing here fails or waits for its caller.
    pub fn write(&self, row: &Row) -> bool {
        match &self.to {
            To::Log(dir) => {
                let log = aterm_telemetry::Log::new(
                    dir.clone(),
                    aterm_telemetry::Limits::DEFAULT,
                    aterm_telemetry::direct::facts().0.clone(),
                )
                .waiting(Duration::ZERO);
                aterm_telemetry::direct::write_lines(&log, &[row.line()], &LOST)
            }
            #[cfg(any(test, debug_assertions))]
            To::Capture => seam::took(row),
        }
    }
}

/// THIS ACCOUNT'S WRITER: the log in [`default_dir`], through which verify writes its rows; the
/// thread's [`capture`] where a test installed one; `None` with no state directory, and in this
/// crate's unit tests.
#[must_use]
pub fn user() -> Option<Writer> {
    #[cfg(any(test, debug_assertions))]
    if let Some(writer) = seam::user() {
        return Some(writer);
    }
    if cfg!(test) {
        return None;
    }
    default_dir().map(Writer::at)
}

/// The writer of `ledger`'s `look` and `admitted` rows: [`user`]'s where `ledger` is this
/// user's own ([`Ledger::user`]'s folder, or the capture's stand-in for it), else none
/// ([`writer_in`]).
fn writer_for(ledger: &Ledger) -> Option<Writer> {
    #[cfg(any(test, debug_assertions))]
    if let Some(found) = seam::writer_for(ledger) {
        return found;
    }
    if cfg!(test) {
        return None;
    }
    let user = Ledger::account_at(crate::ledger::user_dir()?);
    writer_in(ledger, &user, || default_dir().map(Writer::at))
}

/// THE ONE RULE OF WHO WRITES `look` AND `admitted` ROWS, production's and a capture's alike:
/// `ledger`'s rows go to `log`'s writer where `ledger` is `user`, this account's own ledger (the
/// same folder, [`aterm_telemetry::direct::same_folder`]); a scratch ledger's go nowhere, and
/// `log` is not asked for its writer.
fn writer_in(
    ledger: &Ledger,
    user: &Ledger,
    log: impl FnOnce() -> Option<Writer>,
) -> Option<Writer> {
    if !aterm_telemetry::direct::same_folder(ledger.dir(), user.dir()) {
        return None;
    }
    log()
}

/// What a read of the log found ([`read`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// The rows written at or after the read's start, oldest file first.
    pub rows: Vec<Row>,
    /// Rows the writers lost at or after it, as gap lines say.
    pub lost: u64,
    /// Rows lost that no gap line says yet: the loss records in the folder, which the next append
    /// takes.
    pub lost_pending: u64,
    /// Lines that are no row of this log: torn, another schema, or no object.
    pub torn: u64,
    /// Files read.
    pub files: u32,
    /// Bytes read.
    pub bytes: u64,
    /// The oldest line's time, unix ms, whatever the read's start: how far back the log reaches.
    pub oldest_ms: Option<u64>,
    /// The oldest row of each kind the files hold, unix ms, whatever the read's start: how far
    /// back the log reaches for that kind.
    pub oldest_by_kind: std::collections::BTreeMap<String, u64>,
}

/// Read every file of the log in `dir`, oldest first, keeping the rows written at or after
/// `since_ms`. A file that is gone (rotated away meanwhile), or a folder that is, is skipped.
///
/// # Errors
/// A file that exists could not be read.
pub fn read(dir: &Path, since_ms: Option<u64>) -> io::Result<Reading> {
    fn oldest(at: u64, out: &mut Reading) {
        out.oldest_ms = Some(out.oldest_ms.map_or(at, |o| o.min(at)));
    }
    let mut out = Reading::default();
    let since = since_ms.unwrap_or(0);
    for n in (0..aterm_telemetry::Limits::DEFAULT.files).rev() {
        let path = aterm_telemetry::log::file_path(dir, n);
        let bytes = match aterm_uds::atomic_file::read_regular(&path, 64 << 20) {
            Ok((bytes, _)) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        out.files += 1;
        out.bytes += bytes.len() as u64;
        for raw in bytes.split(|b| *b == b'\n') {
            let Ok(line) = std::str::from_utf8(raw) else {
                out.torn += 1;
                continue;
            };
            if line.is_empty() {
                continue;
            }
            if line.starts_with('{') {
                match Row::parse(line) {
                    Some(row) => {
                        oldest(row.at_ms(), &mut out);
                        let kind = out
                            .oldest_by_kind
                            .entry(row.kind().to_owned())
                            .or_insert(u64::MAX);
                        *kind = (*kind).min(row.at_ms());
                        if row.at_ms() >= since {
                            out.rows.push(row);
                        }
                    }
                    None => out.torn += 1,
                }
                continue;
            }
            match aterm_telemetry::row::parse_line(line) {
                Some(aterm_telemetry::row::Line::Host(host)) => oldest(host.at_ms, &mut out),
                Some(aterm_telemetry::row::Line::Gap { at_ms, dropped }) => {
                    if at_ms >= since {
                        out.lost = out.lost.saturating_add(dropped);
                    }
                }
                _ => out.torn += 1,
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(rows) = entry.file_name().to_str().and_then(lost_rows) {
                out.lost_pending = out.lost_pending.saturating_add(rows);
            }
        }
    }
    Ok(out)
}

/// The rows a loss record's name holds (`lost-<writer>-<rows>`,
/// [`aterm_telemetry::log::mark_lost`]).
fn lost_rows(name: &str) -> Option<u64> {
    let (writer, rows) = name.strip_prefix("lost-")?.rsplit_once('-')?;
    (!writer.is_empty() && !rows.starts_with('+'))
        .then(|| rows.parse::<u32>().ok())
        .flatten()
        .map(u64::from)
}

/// A claim's id in the log: its place in the queue ([`Record::order`]) as its file's stem,
/// `<start_ms>-<pid>-<seq>`, which verify's `stage` rows name it by too.
#[must_use]
pub fn id_of(order: (u64, u32, u64)) -> String {
    format!("{}-{}-{}", order.0, order.1, order.2)
}

/// The word a row says a shortfall by.
#[must_use]
pub const fn short_word(short: &Short) -> &'static str {
    match short {
        Short::Release => "release",
        Short::Cores => "cores",
        Short::Timing(_) => "timing",
        Short::Memory { .. } => "memory",
        Short::Disk { .. } => "disk",
        Short::Load { .. } => "load",
        Short::Gpu { .. } => "gpu",
        Short::Quiet { .. } => "quiet",
        Short::Reserved { .. } => "reserved",
        Short::Pressure => "pressure",
        Short::Alone { .. } => "alone",
        Short::WantsAlone { .. } => "wants-alone",
        Short::Heavy { .. } => "heavy",
        Short::Turn { .. } => "turn",
        Short::Ledger(_) => "ledger",
    }
}

/// Every word [`short_word`] says, each a bit of [`Looks::seen`] by its place here.
const WORDS: [&str; 15] = [
    "release",
    "cores",
    "timing",
    "memory",
    "disk",
    "load",
    "gpu",
    "quiet",
    "reserved",
    "pressure",
    "alone",
    "wants-alone",
    "turn",
    "ledger",
    "heavy",
];

/// `short`'s bit in [`Looks::seen`].
fn bit(short: &Short) -> u16 {
    let word = short_word(short);
    WORDS
        .iter()
        .position(|w| *w == word)
        .map_or(0, |at| 1 << at)
}

/// Whether `short` is one a `look` row is written for: memory, another claim's memory held
/// ALONE, ALONE asked, or the kernel's pressure.
#[must_use]
pub const fn memory_class(short: &Short) -> bool {
    matches!(
        short,
        Short::Memory { .. } | Short::Alone { .. } | Short::WantsAlone { .. } | Short::Pressure
    )
}

/// WHAT A WAITER'S LOOKS HAVE FOUND, FOR ITS `admitted` ROW: the kinds of shortfall any of its
/// looks found (a look that met another admission's lock, or could not read the ledger, among
/// them), and whether a forced look admitted it. A waiter keeps it across its looks and its claim
/// takes it at its grant. When `look` rows are due is the wait's, not the waiter's ([`Waits`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Looks {
    /// Each kind of shortfall a look found, a bit by its place in [`WORDS`].
    seen: u16,
    /// A forced look (`--admit now`) admitted it.
    forced: bool,
}

impl Looks {
    /// These looks, admitted by a forced look or not.
    pub(crate) const fn admitted(self, forced: bool) -> Looks {
        Looks { forced, ..self }
    }

    /// Whether a forced look admitted it.
    pub(crate) const fn forced(self) -> bool {
        self.forced
    }

    /// The words of every kind of shortfall its looks found.
    fn words(self) -> Vec<&'static str> {
        WORDS
            .iter()
            .enumerate()
            .filter(|(at, _)| self.seen & (1 << at) != 0)
            .map(|(_, word)| *word)
            .collect()
    }

    /// Book each kind of shortfall `short` holds.
    fn book(&mut self, short: &Shortfall) {
        for s in &short.shorts {
            self.seen |= bit(s);
        }
    }
}

impl crate::Waiter {
    /// This waiter, back from a look that read no ledger (it met another admission's lock, or
    /// could not read the ledger), short of `short` alone: booked among its looks' kinds
    /// ([`Looks`]), with no `look` row due, since the look read nothing it could say.
    pub(crate) fn unread(self, short: Short) -> Polled {
        let (waiter, short) = self.booked(short);
        Polled::Waiting(waiter, short)
    }

    /// [`Self::unread`], for a forced look ([`crate::Waiter::force`]): it decided nothing.
    pub(crate) fn unread_forced(self, short: Short) -> crate::Forced {
        let (waiter, short) = self.booked(short);
        crate::Forced::Turn(waiter, short)
    }

    /// This waiter with `short` booked among its looks' kinds, and the shortfall of it alone.
    fn booked(mut self, short: Short) -> (crate::Waiter, Shortfall) {
        let short = Shortfall {
            shorts: vec![short],
            holders: Vec::new(),
        };
        self.looks.book(&short);
        (self, short)
    }
}

/// WHICH WAIT A LOOK BELONGS TO ([`Waits`]): its ledger's folder, and its need's words and
/// memory. Two waiters with one key in one process are one wait: the same need entered again
/// (a caller that gives up its queue place between looks), or two needs alike in all three.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WaitKey {
    ledger: PathBuf,
    what: String,
    mem: u64,
}

impl WaitKey {
    fn of(ledger: &Ledger, record: &Record) -> WaitKey {
        WaitKey {
            ledger: ledger.dir().to_path_buf(),
            what: record.need.what.clone(),
            mem: record.need.mem,
        }
    }
}

/// One wait that wrote a `look` row and has not ended ([`Waits`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Open {
    key: WaitKey,
    /// Its id in the log: the claim whose look wrote its first row ([`id_of`]).
    id: String,
    /// When its last `look` row was due, unix ms.
    wrote_ms: u64,
    /// When a look of it was last seen, unix ms.
    seen_ms: u64,
}

/// THE WAITS THIS PROCESS'S LOOKS HAVE LOGGED AND NOT YET ENDED, by what waits ([`WaitKey`])
/// rather than by the waiter value that looks: so a caller that drops its waiter between looks
/// and enters a fresh one at the next (a compiler unit's wait, which gives up its queue place;
/// a test binary that is not the dispatch's queued one) writes one `look` row a minute under one
/// id, as a waiter kept across its looks does. A wait ends at the look that admits it, or once no
/// look of it has been seen for [`SILENT_ENDS`]; at most [`WAITS_KEPT`] are kept.
#[derive(Debug, Default)]
pub(crate) struct Waits(Vec<Open>);

impl Waits {
    /// Forget each wait no look has been seen of for [`SILENT_ENDS`] by `now`, then the ones
    /// silent longest past [`WAITS_KEPT`].
    fn prune(&mut self, now: u64) {
        self.0
            .retain(|open| now.saturating_sub(open.seen_ms) < SILENT_ENDS);
        if self.0.len() > WAITS_KEPT {
            self.0.sort_by_key(|open| std::cmp::Reverse(open.seen_ms));
            self.0.truncate(WAITS_KEPT);
        }
    }

    /// A look at `now` of the wait `key` by the claim `claim` ([`id_of`]), short of memory's
    /// kinds (`memory`) or not: the wait's id where a `look` row is due, at its first memory
    /// look and then once [`LOOK_EVERY`] has passed since its last; `None` otherwise, its wait
    /// kept open where it is one.
    fn look(&mut self, key: &WaitKey, claim: &str, now: u64, memory: bool) -> Option<String> {
        self.prune(now);
        let open = self.0.iter_mut().find(|open| open.key == *key);
        match open {
            Some(open) => {
                open.seen_ms = now;
                if !memory || now.saturating_sub(open.wrote_ms) < LOOK_EVERY {
                    return None;
                }
                open.wrote_ms = now;
                Some(open.id.clone())
            }
            None if memory => {
                self.0.push(Open {
                    key: key.clone(),
                    id: claim.to_owned(),
                    wrote_ms: now,
                    seen_ms: now,
                });
                Some(claim.to_owned())
            }
            None => None,
        }
    }

    /// Whether the wait `key` is open at `now`.
    fn open(&mut self, key: &WaitKey, now: u64) -> bool {
        self.prune(now);
        self.0.iter().any(|open| open.key == *key)
    }

    /// The wait `key`, admitted at `now`: its id where it is open, and it ends.
    fn admitted(&mut self, key: &WaitKey, now: u64) -> Option<String> {
        self.prune(now);
        let at = self.0.iter().position(|open| open.key == *key)?;
        Some(self.0.remove(at).id)
    }
}

/// This process's open waits ([`Waits`]), each lock of it held for one call of its methods.
static WAITS: Mutex<Waits> = Mutex::new(Waits(Vec::new()));

/// A WAITER'S LOOK, BEFORE ITS DECISION MOVES IT: its ledger where its wait is open ([`Waits`])
/// and so owes a `look` row at its admission, since a grant consumes the waiter.
pub(crate) struct Before {
    ledger: Option<Ledger>,
}

impl Before {
    /// `waiter`'s look, before its decision: its ledger kept only where its wait is open.
    pub(crate) fn of(waiter: &crate::Waiter) -> Before {
        let key = WaitKey::of(&waiter.ledger, waiter.record());
        Before {
            ledger: WAITS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .open(&key, aterm_telemetry::now_ms())
                .then(|| waiter.ledger.clone()),
        }
    }
}

/// One held claim of memory a look was held to ([`Seen`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldMem {
    order: (u64, u32, u64),
    /// What it claims now ([`crate::Need::mem_now`]).
    claimed: u64,
    /// What it still reserves, by the look's own reckoning ([`crate::Reserve::mem`]).
    reserved: u64,
    /// Whether it holds the machine's memory alone.
    alone: bool,
}

/// WHAT A LOOK READ THAT ITS `look` ROW SAYS BESIDE ITS SHORTFALL: what it read available, and,
/// where it was short of memory's kinds ([`memory_class`]), every held claim of memory among the
/// claims it was held to, whatever its shortfall's holders name: a look short only of the
/// kernel's pressure, or of a claim held ALONE, names none or only that one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    available: Option<u64>,
    held: Vec<HeldMem>,
}

impl Seen {
    /// What a look that read `available` saw, held to `others` (the claims [`crate::admit::decide`]
    /// read), short of `short` where it did not admit: each held claim of memory among them, with
    /// what it still reserves reckoned as that decision reckons it ([`crate::admit::reservations`]
    /// over the held claims, which it credits first), where `short` is of memory's kinds.
    pub(crate) fn of(
        available: Option<u64>,
        others: &[crate::admit::Other<'_>],
        short: Option<&Shortfall>,
    ) -> Seen {
        if !short.is_some_and(|short| short.shorts.iter().any(memory_class)) {
            return Seen {
                available,
                held: Vec::new(),
            };
        }
        let held: Vec<&crate::admit::Other<'_>> =
            others.iter().filter(|o| o.record.held.is_some()).collect();
        let reserves = crate::admit::reservations(&held, None);
        let held = held
            .iter()
            .zip(&reserves)
            .filter(|(o, _)| o.record.need.mem > 0)
            .map(|(o, r)| HeldMem {
                order: o.record.order(),
                claimed: o.record.need.mem_now(),
                reserved: r.mem,
                alone: o.record.held.is_some_and(|h| h.alone),
            })
            .collect();
        Seen { available, held }
    }
}

/// After a look that decided `polled`, with the ledger's lock let go: book what it was short of
/// in the waiter's [`Looks`], and write its `look` row where one is due for its wait ([`Waits`],
/// [`LOOK_EVERY`]), saying what the look `seen` read.
pub(crate) fn looked(before: Before, polled: &mut Polled, seen: &Seen) {
    let now = aterm_telemetry::now_ms();
    match polled {
        Polled::Waiting(waiter, short) => waiting(waiter, short, seen, now),
        Polled::Admitted(claim) => admitting(&before, claim, seen, now),
        Polled::Gone(_) | Polled::Unqualified(_) => {}
    }
}

/// [`looked`] for a forced look ([`crate::Waiter::force`]): the same rows, for the claim it
/// admitted or the waiter it handed back.
pub(crate) fn forced(before: Before, forced: &mut Result<crate::Forced, Error>, seen: &Seen) {
    let now = aterm_telemetry::now_ms();
    match forced {
        Ok(crate::Forced::Turn(waiter, short)) => waiting(waiter, short, seen, now),
        Ok(crate::Forced::Admitted(claim, _)) => admitting(&before, claim, seen, now),
        Err(_) => {}
    }
}

/// A look at `now` that left `waiter` short of `short`: booked, and its wait's row where due.
fn waiting(waiter: &mut crate::Waiter, short: &Shortfall, seen: &Seen, now: u64) {
    waiter.looks.book(short);
    let memory = short.shorts.iter().any(memory_class);
    let claim = id_of(waiter.record().order());
    let key = WaitKey::of(&waiter.ledger, waiter.record());
    let Some(wait) = WAITS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .look(&key, &claim, now, memory)
    else {
        return;
    };
    if let Some(writer) = writer_for(&waiter.ledger) {
        writer.write(&look_row(now, &wait, waiter.record(), Some(short), seen));
    }
}

/// A look at `now` that admitted `claim`: the admitting row of its wait, where it is open.
fn admitting(before: &Before, claim: &Claim, seen: &Seen, now: u64) {
    let Some(ledger) = before.ledger.as_ref() else {
        return;
    };
    let Some(wait) = WAITS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .admitted(&WaitKey::of(ledger, claim.record()), now)
    else {
        return;
    };
    if let Some(writer) = writer_for(ledger) {
        writer.write(&look_row(now, &wait, claim.record(), None, seen));
    }
}

/// The `look` row at `at_ms` of the wait `wait`, looked at by the claim `record`: its claim
/// (`claim`, where it is not the one the wait is named by), what the look read available (`seen`),
/// and, waiting (`short`), what it is short of, each claim its shortfall names and every held
/// claim of memory; admitted where `short` is `None`.
fn look_row(
    at_ms: u64,
    wait: &str,
    record: &Record,
    short: Option<&Shortfall>,
    seen: &Seen,
) -> Row {
    let claim = id_of(record.order());
    let mut row = claim_fields(Row::new("look", at_ms), record)
        .with("id", wait)
        .with("admitted", short.is_none())
        .with("available", seen.available);
    if claim != wait {
        row = row.with("claim", claim);
    }
    let Some(short) = short else {
        return row.with("shorts", Vec::<Value>::new());
    };
    let words: Vec<Value> = short
        .shorts
        .iter()
        .map(|s| Value::from(short_word(s)))
        .collect();
    row = row.with("shorts", words);
    if let Some(Short::Memory {
        reserved, passes, ..
    }) = short
        .shorts
        .iter()
        .find(|s| matches!(s, Short::Memory { .. }))
    {
        row = row.with("reserved", *reserved).with("passes", *passes);
    }
    let holders: Vec<Value> = short
        .holders
        .iter()
        .map(|h| {
            let mut fields = Map::new();
            fields.insert("id".to_owned(), Value::from(id_of(h.order)));
            fields.insert("pid".to_owned(), Value::from(h.pid));
            fields.insert("held".to_owned(), Value::from(h.held));
            fields.insert("claimed".to_owned(), Value::from(h.claimed));
            fields.insert("reserved".to_owned(), Value::from(h.mem));
            fields.insert("alone".to_owned(), Value::from(h.alone));
            Value::Object(fields)
        })
        .collect();
    let held: Vec<Value> = seen
        .held
        .iter()
        .map(|h| {
            let mut fields = Map::new();
            fields.insert("id".to_owned(), Value::from(id_of(h.order)));
            fields.insert("claimed".to_owned(), Value::from(h.claimed));
            fields.insert("reserved".to_owned(), Value::from(h.reserved));
            fields.insert("alone".to_owned(), Value::from(h.alone));
            Value::Object(fields)
        })
        .collect();
    row.with("holders", holders).with("held_mem", held)
}

/// `row` with the fields of the claim `record`: its id, holder, words, rank and memory need.
fn claim_fields(row: Row, record: &Record) -> Row {
    row.with("id", id_of(record.order()))
        .with("pid", record.pid)
        .with("what", record.need.what.as_str())
        .with(
            "rank",
            match record.need.rank {
                crate::Rank::Release => "release",
                crate::Rank::Ordinary => "ordinary",
                crate::Rank::Background => "background",
            },
        )
        .with("mem", record.need.mem)
        .with("margin", record.need.margin)
        .with("run_mem", record.need.run_mem)
}

/// An admission's decision in `ledger`: where it admitted a claim and `ledger` is this user's
/// own, its `admitted` row.
pub(crate) fn decided(ledger: &Ledger, result: Result<&Claim, &Error>) {
    let Ok(claim) = result else {
        return;
    };
    let Some(writer) = writer_for(ledger) else {
        return;
    };
    writer.write(&admitted_row(aterm_telemetry::now_ms(), claim));
}

/// The `admitted` row of `claim`, written at `at_ms`.
fn admitted_row(at_ms: u64, claim: &Claim) -> Row {
    let record = claim.record();
    let words: Vec<Value> = claim.looks.words().into_iter().map(Value::from).collect();
    claim_fields(Row::new("admitted", at_ms), record)
        .with("asked_ms", claim.asked_ms())
        .with("admitted_ms", record.held.map(|h| h.admitted_ms))
        .with("cores", record.held.map(|h| h.cores))
        .with("alone", record.held.is_some_and(|h| h.alone))
        .with("forced", claim.looks.forced())
        .with("shorts", words)
}

/// TEST SEAM (test and debug builds), past the unit-test build's silence and into a dependent's
/// tests: run `f` on this thread with `user` standing for this user's ledger ([`Ledger::user`]),
/// and return what it returned with every row written meanwhile on this thread, in order: the
/// `look` and `admitted` rows of `user`'s admissions, and every row written through [`user`]'s
/// writer. None goes to this account's folder; an admission in another ledger writes none, by the
/// rule production follows (`writer_in`). A capture inside another takes the rows written while
/// it runs.
#[cfg(any(test, debug_assertions))]
pub fn capture<T>(user: Ledger, f: impl FnOnce() -> T) -> (T, Vec<Row>) {
    seam::install(user, seam::To::Rows(Vec::new()), f)
}

/// [`capture`], its rows written to the log in `dir` instead of taken (a folder no write can
/// make, to read how a lost row is counted).
#[cfg(any(test, debug_assertions))]
pub fn capture_in<T>(user: Ledger, dir: PathBuf, f: impl FnOnce() -> T) -> T {
    seam::install(user, seam::To::Dir(dir), f).0
}

/// [`capture`]'s per-thread stand-in.
#[cfg(any(test, debug_assertions))]
mod seam {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::{Row, Writer};
    use crate::Ledger;

    /// Where a capture's rows go.
    pub(super) enum To {
        /// Taken, in order.
        Rows(Vec<Row>),
        /// Written to the log in this folder.
        Dir(PathBuf),
    }

    /// A capture's stand-in for this user's ledger, and where its rows go.
    struct Seam {
        user: Ledger,
        to: To,
    }

    impl Seam {
        fn writer(&self) -> Writer {
            match &self.to {
                To::Rows(_) => Writer {
                    to: super::To::Capture,
                },
                To::Dir(dir) => Writer::at(dir.clone()),
            }
        }
    }

    thread_local! {
    // Behind a `Box`: every thread's copy of the composed app's thread-local template is held to
    // 8 KiB (`crates/aterm/tests/native_thread_local_template.rs`), and a boxed seam costs each
    // thread a pointer and the cell's borrow flag, not its whole value.
        static SEAM: RefCell<Option<Box<Seam>>> = const { RefCell::new(None) };
    }

    pub(super) fn install<T>(user: Ledger, to: To, f: impl FnOnce() -> T) -> (T, Vec<Row>) {
        /// This capture, installed over the one it replaced (none, or an outer capture), which
        /// it puts back however `f` ends.
        struct Installed(Option<Option<Box<Seam>>>);
        impl Installed {
            fn restore(&mut self) -> Option<Box<Seam>> {
                let outer = self.0.take()?;
                SEAM.with(|seam| std::mem::replace(&mut *seam.borrow_mut(), outer))
            }
        }
        impl Drop for Installed {
            fn drop(&mut self) {
                let _ = self.restore();
            }
        }
        let outer = SEAM.with(|seam| seam.borrow_mut().replace(Box::new(Seam { user, to })));
        let mut installed = Installed(Some(outer));
        let out = f();
        let rows = match installed.restore().map(|seam| seam.to) {
            Some(To::Rows(rows)) => rows,
            _ => Vec::new(),
        };
        (out, rows)
    }

    /// The capture's writer, where one is installed on this thread.
    pub(super) fn user() -> Option<Writer> {
        SEAM.with(|seam| seam.try_borrow().ok()?.as_deref().map(Seam::writer))
    }

    /// Where one is installed on this thread: its writer for `ledger`'s rows where `ledger` is
    /// its stand-in for this user's, else none ([`super::writer_in`]).
    pub(super) fn writer_for(ledger: &Ledger) -> Option<Option<Writer>> {
        SEAM.with(|seam| {
            let seam = seam.try_borrow().ok()?;
            let seam = seam.as_ref()?;
            Some(super::writer_in(ledger, &seam.user, || Some(seam.writer())))
        })
    }

    /// Take `row` into this thread's capture: whether one took it.
    pub(super) fn took(row: &Row) -> bool {
        SEAM.with(|seam| {
            let Ok(mut seam) = seam.try_borrow_mut() else {
                return false;
            };
            match seam.as_mut().map(|seam| &mut seam.to) {
                Some(To::Rows(rows)) => {
                    rows.push(row.clone());
                    true
                }
                _ => false,
            }
        })
    }
}

#[cfg(test)]
#[path = "admission_log_tests.rs"]
mod tests;
