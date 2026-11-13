// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account state of a login no roster lists (`docs/DESIGN-account-discovery-2026-11-09.md`,
//! step 2): what a CLI's own login store names, whether that login works, and its week.
//!
//! [`super::accounts::Store`] keeps its state by roster label, so an account no row lists has
//! no record there, whatever CLI it is. Muse Code is the first CLI all of whose accounts are
//! such: aterm keeps no pool of its logins, so its one login per config home is known by
//! observation alone. This store is that login's record on this machine, keyed by the login
//! file and the address it names, and the one source of what every reader says of it (the
//! footer, `aterm harness accounts`, the survey), and of what the journal hears:
//!
//! - [`Store::observe`] takes what the login file says ([`super::observe::muse`]): a file that
//!   names no login signs out the account it last named (`signed-out`, sharp), and a file
//!   rewritten since a sign-out makes it unseen again (a new sign-in).
//! - [`Store::admit`] is the gate of a background read (owner rulings 2026-11-09: "if you're
//!   not logged in, then you can't return this value, and some other hook should handle this"):
//!   a login on record as signed out gets no [`Pass`], so no process is started for it.
//! - [`Store::landed`] records what the read said ("failing auth should update the known local
//!   and fleet state of that account"): a reading is `auth ok` and its week, the vendor's
//!   sign-in words are `signed-out`, anything else a failed read that holds the next
//!   background read for [`FAILED_HOLD_S`], across sessions and restarts.
//!
//! The record is machine-local (mode 0600, as the rosters that hold addresses are). The journal
//! gets the account by its key, never its address.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::{Value, json};

use super::accounts::Auth;
use super::journal::{self, LoginState, Note, ObservedCli, ObservedLogin, ObservedSrc};
use super::observe::Observation;
use super::profile::WeeklyFigure;

/// The directory under the harness state that holds one file per CLI.
pub const DIR: &str = "observed-accounts";

const SCHEMA: u64 = 1;

/// The most records one CLI's file keeps: the oldest signed-out ones go first.
const RECORDS_MAX: usize = 64;

/// The largest record file read.
const FILE_MAX: u64 = 256 * 1024;

/// How far apart two reads of one login are, whoever asks: the vendor mints a fresh figure at
/// most this often, and a read another window started within it is still in flight.
pub const READ_APART_S: i64 = 60;

/// How long a read that gave nothing and named no sign-in wall holds the next background read
/// of that login. A person's own ask ([`Store::ask`]) is not held by it.
pub const FAILED_HOLD_S: i64 = 3_600;

/// The bounded wait for the record's lock: a writer holds it for one small file's rewrite.
const LADDER: [Duration; 3] = [
    Duration::from_millis(10),
    Duration::from_millis(40),
    Duration::from_millis(150),
];

/// One login's record: the account a login file names on this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The login email the file names.
    pub email: String,
    /// The login file.
    pub place: PathBuf,
    /// Whether the login works, as last learned.
    pub auth: Auth,
    /// When [`Self::auth`] last changed, Unix seconds.
    pub auth_at: Option<i64>,
    /// When the login file was last seen naming it, to the hour.
    pub seen_at: Option<i64>,
    /// Its plan's words, where a read named them.
    pub plan: Option<String>,
    /// Its week, where a read gave a figure.
    pub week: Option<WeeklyFigure>,
    /// When a read of it was last admitted.
    pub read_at: Option<i64>,
    /// When a read of it last gave nothing; cleared by a reading.
    pub failed_at: Option<i64>,
    /// A person asked for a fresh read ([`Store::ask`]); cleared when one is admitted.
    pub asked: bool,
}

/// Why a read of a login is not admitted ([`Store::admit`], [`Record::standing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The login is on record as signed out since `since`.
    SignedOut {
        /// The account's address.
        email: String,
        /// When that was recorded, Unix seconds.
        since: Option<i64>,
    },
    /// A read of it was admitted within [`READ_APART_S`].
    Recent,
    /// Its last read gave nothing, within [`FAILED_HOLD_S`].
    Failed,
    /// No record of this login: its file was not observed first.
    Unlisted,
    /// The record could not be locked or written.
    Unwritable,
}

impl Refusal {
    /// The refusal in a person's words, the account by its address.
    #[must_use]
    pub fn words(&self, cli: ObservedCli) -> String {
        let cli = cli.id();
        match self {
            Self::SignedOut { email, since } => format!(
                "signed-out: {cli}'s login {email} is on record as signed out{}, so {cli} is not \
                 run for a read (a read never starts a sign-in); sign in to {cli} in a tab",
                since.map_or_else(String::new, |at| format!(
                    " since {}",
                    super::usage::rfc3339_utc(at)
                ))
            ),
            Self::Recent => format!("a read of this {cli} login ran within the last minute"),
            Self::Failed => format!(
                "the last read of this {cli} login gave nothing; the next background read waits \
                 an hour from it"
            ),
            Self::Unlisted => format!("no {cli} login is on record here"),
            Self::Unwritable => format!("{cli}'s account record could not be written"),
        }
    }
}

impl Record {
    /// Whether a read of this login may start at `now`; `asked` is a person's own ask, which a
    /// failed read does not hold. Nothing here runs anything.
    ///
    /// # Errors
    ///
    /// The login is signed out, was read within the minute, or its last read failed.
    pub fn standing(&self, asked: bool, now: i64) -> Result<(), Refusal> {
        if self.auth == Auth::SignedOut {
            return Err(Refusal::SignedOut {
                email: self.email.clone(),
                since: self.auth_at,
            });
        }
        let within = |at: Option<i64>, hold: i64| {
            at.is_some_and(|at| (0..hold).contains(&now.saturating_sub(at)))
        };
        if within(self.read_at, READ_APART_S) {
            return Err(Refusal::Recent);
        }
        if !asked && within(self.failed_at, FAILED_HOLD_S) {
            return Err(Refusal::Failed);
        }
        Ok(())
    }

    fn is(&self, place: &Path, email: &str) -> bool {
        self.place == place && self.email.eq_ignore_ascii_case(email.trim())
    }

    fn to_json(&self) -> Value {
        json!({
            "email": self.email,
            "place": self.place.to_string_lossy().into_owned(),
            "auth": self.auth.as_str(),
            "auth_at": self.auth_at,
            "seen_at": self.seen_at,
            "plan": self.plan,
            "week": self.week.map(|week| json!({
                "used_x10": week.used_x10,
                "resets_at": week.resets_at,
                "at": week.at,
            })),
            "read_at": self.read_at,
            "failed_at": self.failed_at,
            "asked": self.asked,
        })
    }

    fn of_json(value: &Value) -> Option<Record> {
        let text = |key: &str| value.get(key).and_then(Value::as_str);
        let secs = |key: &str| value.get(key).and_then(Value::as_i64);
        let email = text("email")?.to_owned();
        let place = PathBuf::from(text("place")?);
        if super::accounts::email_refusal(&email).is_some() || !place.is_absolute() {
            return None;
        }
        let week = value.get("week").and_then(|week| {
            Some(WeeklyFigure {
                used_x10: u32::try_from(week.get("used_x10")?.as_u64()?)
                    .ok()?
                    .min(1000),
                resets_at: week.get("resets_at")?.as_i64()?,
                at: week.get("at")?.as_i64()?,
            })
        });
        Some(Record {
            email,
            place,
            auth: text("auth").map_or(Auth::Unseen, Auth::parse),
            auth_at: secs("auth_at"),
            seen_at: secs("seen_at"),
            plan: text("plan")
                .map(|plan| super::one_line(plan, 48))
                .filter(|plan| !plan.is_empty()),
            week,
            read_at: secs("read_at"),
            failed_at: secs("failed_at"),
            asked: value.get("asked").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// What a read of a login said ([`Store::landed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// The provider answered for this login: its plan and week, where it named them.
    Read {
        /// The plan's words.
        plan: Option<String>,
        /// The week's figure.
        week: Option<WeeklyFigure>,
    },
    /// The vendor's own words said the login is gone.
    SignedOut,
    /// It gave nothing, and no sign-in words.
    Nothing,
}

/// Leave to start one read of one login, made only by [`Store::admit`] and spent by
/// [`Store::landed`].
#[derive(Debug)]
pub struct Pass {
    place: PathBuf,
    email: String,
    at: i64,
}

impl Pass {
    /// When the gate was passed, Unix seconds.
    #[must_use]
    pub const fn at(&self) -> i64 {
        self.at
    }

    /// The address of the login it admits.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }
}

/// One thing a record learned, as the journal is told it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Learned {
    SignedOut { email: String, src: &'static str },
    Unseen { email: String },
    Works { email: String },
    Week { email: String, week: WeeklyFigure },
    Failed { email: String },
}

/// The journal's key of `cli`'s observed login `email` ([`journal::observed_claims`]), through
/// the installed sink or a scoped test keyer.
#[must_use]
pub fn account_key(cli: ObservedCli, email: &str) -> Option<journal::Key> {
    let (realm, email) = journal::observed_claims(cli, None, email)?;
    journal::realm_key(&realm, &email)
}

/// What a change to the record tells the journal and the registry ([`Told::publish`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    /// The journal's notes, each keyed by the observed account.
    pub notes: Vec<Note>,
    /// The logins handed to the registry's `account-obs` rows.
    pub logins: Vec<ObservedLogin>,
}

impl Told {
    /// Tell the journal and the registry, through the installed sink (nothing where none is).
    pub fn publish(&self) {
        journal::publish(&self.notes);
        for login in &self.logins {
            journal::observed(login);
        }
    }

    fn of(cli: ObservedCli, learned: &[Learned], now: i64) -> Told {
        let mut told = Told::default();
        let Ok(t) = u64::try_from(now) else {
            return told;
        };
        for each in learned {
            let (Learned::SignedOut { email, .. }
            | Learned::Unseen { email }
            | Learned::Works { email }
            | Learned::Week { email, .. }
            | Learned::Failed { email }) = each;
            let Some(key) = account_key(cli, email) else {
                continue;
            };
            match each {
                Learned::SignedOut { src, .. } => {
                    told.notes
                        .push(journal::auth_note(cli, key, Auth::SignedOut, src, t));
                    if let Some((realm, email)) = journal::observed_claims(cli, None, email) {
                        told.logins.push(ObservedLogin {
                            cli,
                            realm,
                            email,
                            state: LoginState::SignedOut,
                            tab: None,
                            at: t,
                            src: ObservedSrc::AuthFile,
                        });
                    }
                }
                Learned::Unseen { .. } => told.notes.push(journal::auth_note(
                    cli,
                    key,
                    Auth::Unseen,
                    ObservedSrc::AuthFile.word(),
                    t,
                )),
                Learned::Works { .. } => {
                    told.notes
                        .push(journal::auth_note(cli, key, Auth::Ok, "probe", t));
                }
                Learned::Week { week, .. } => {
                    told.notes.extend(journal::week_note(cli, key, week));
                }
                Learned::Failed { .. } => {
                    told.notes.push(journal::read_failed_note(cli, key, t));
                }
            }
        }
        told
    }
}

/// What the login file says, applied to `records` at `now`: whether anything changed, and what
/// was learned. The record of a seen login is made where none is.
fn observed(records: &mut Vec<Record>, seen: &Observation, now: i64) -> (bool, Vec<Learned>) {
    let mut learned = Vec::new();
    let mut changed = false;
    let sign_out = |record: &mut Record, learned: &mut Vec<Learned>| {
        record.auth = Auth::SignedOut;
        record.auth_at = Some(now);
        record.asked = false;
        learned.push(Learned::SignedOut {
            email: record.email.clone(),
            src: ObservedSrc::AuthFile.word(),
        });
    };
    match seen {
        // A key-only login or one off the file names no address this record keeps.
        Observation::Unread | Observation::KeyOnly { .. } | Observation::Routed { .. } => {}
        Observation::SignedOut { path } => {
            for record in records
                .iter_mut()
                .filter(|record| record.place == *path && record.auth != Auth::SignedOut)
            {
                sign_out(record, &mut learned);
                changed = true;
            }
        }
        Observation::Seen(login) => {
            // The store now names this login: any other it named before is gone from it.
            for record in records.iter_mut().filter(|record| {
                record.place == login.path
                    && !record.is(&login.path, &login.email)
                    && record.auth != Auth::SignedOut
            }) {
                sign_out(record, &mut learned);
                changed = true;
            }
            let at = match records
                .iter()
                .position(|record| record.is(&login.path, &login.email))
            {
                Some(at) => at,
                None => {
                    records.push(Record {
                        email: login.email.trim().to_owned(),
                        place: login.path.clone(),
                        ..Record::default()
                    });
                    changed = true;
                    records.len() - 1
                }
            };
            let Some(record) = records.get_mut(at) else {
                return (changed, learned);
            };
            // A file written since the sign-out was recorded is a new sign-in: nothing is known
            // of it until a read says.
            if record.auth == Auth::SignedOut
                && let (Some(written), Some(since)) = (login.mtime, record.auth_at)
                && written >= since
            {
                record.auth = Auth::Unseen;
                record.auth_at = Some(now);
                record.failed_at = None;
                record.read_at = None;
                learned.push(Learned::Unseen {
                    email: record.email.clone(),
                });
                changed = true;
            }
            if record
                .seen_at
                .is_none_or(|at| now.saturating_sub(at) >= super::observe::SEEN_REFRESH_S)
            {
                record.seen_at = Some(now);
                changed = true;
            }
        }
    }
    (changed, learned)
}

/// What a read said, applied to `record` at `now`.
fn landed(record: &mut Record, landing: Landing, now: i64) -> Vec<Learned> {
    let mut learned = Vec::new();
    let email = record.email.clone();
    match landing {
        Landing::Read { plan, week } => {
            if record.auth != Auth::Ok {
                record.auth = Auth::Ok;
                record.auth_at = Some(now);
                learned.push(Learned::Works {
                    email: email.clone(),
                });
            }
            record.failed_at = None;
            if plan.is_some() {
                record.plan = plan;
            }
            if let Some(week) = week {
                if record.week != Some(week) {
                    learned.push(Learned::Week { email, week });
                }
                record.week = Some(week);
            }
        }
        Landing::SignedOut => {
            if record.auth != Auth::SignedOut {
                learned.push(Learned::SignedOut {
                    email,
                    src: "probe",
                });
            }
            record.auth = Auth::SignedOut;
            record.auth_at = Some(now);
            record.asked = false;
        }
        Landing::Nothing => {
            record.failed_at = Some(now);
            learned.push(Learned::Failed { email });
        }
    }
    learned
}

/// Where one CLI's observed-account records live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
    cli: ObservedCli,
}

impl Store {
    /// The store of `cli`'s observed accounts under the harness state `state`.
    #[must_use]
    pub fn at(state: &Path, cli: ObservedCli) -> Store {
        Store {
            dir: state.join(DIR),
            cli,
        }
    }

    /// [`Store::at`] this machine's harness state ([`super::cli::harness_state_dir`]); `None`
    /// where no state directory is known.
    #[must_use]
    pub fn of_machine(cli: ObservedCli) -> Option<Store> {
        Some(Store::at(&super::cli::harness_state_dir().ok()?, cli))
    }

    /// The CLI whose logins it records.
    #[must_use]
    pub const fn cli(&self) -> ObservedCli {
        self.cli
    }

    fn file(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.cli.id()))
    }

    fn lock(&self) -> Option<super::accounts::Lock> {
        std::fs::create_dir_all(&self.dir).ok()?;
        super::accounts::lock_at_within(&self.dir.join(format!("{}.lock", self.cli.id())), &LADDER)
            .ok()
            .flatten()
    }

    /// Every record, as last written: an absent or unreadable file is none.
    #[must_use]
    pub fn kept(&self) -> Vec<Record> {
        let Ok(Some(text)) = super::accounts::read_bounded(&self.file(), FILE_MAX) else {
            return Vec::new();
        };
        let Ok(value) = aterm_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        if value.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return Vec::new();
        }
        value
            .get("logins")
            .and_then(Value::as_array)
            .map(|logins| logins.iter().filter_map(Record::of_json).collect())
            .unwrap_or_default()
    }

    /// The record of the login `place` names as `email`.
    #[must_use]
    pub fn record(&self, place: &Path, email: &str) -> Option<Record> {
        self.kept()
            .into_iter()
            .find(|record| record.is(place, email))
    }

    fn save(&self, mut records: Vec<Record>) -> std::io::Result<()> {
        // A login whose config home is gone is no login of this machine's any more.
        records.retain(|record| record.place.parent().is_some_and(Path::is_dir));
        if records.len() > RECORDS_MAX {
            records.sort_by_key(|record| {
                (
                    record.auth != Auth::SignedOut,
                    record.auth_at.max(record.seen_at),
                )
            });
            records.drain(..records.len() - RECORDS_MAX);
        }
        records.sort_by(|a, b| (&a.place, &a.email).cmp(&(&b.place, &b.email)));
        let value = json!({
            "schema": SCHEMA,
            "logins": records.iter().map(Record::to_json).collect::<Vec<_>>(),
        });
        let mut text = aterm_json::to_string(&value).map_err(std::io::Error::other)?;
        text.push('\n');
        super::atomic_write::write_atomic_mode(&self.file(), &text, Some(0o600))
    }

    /// Edit the records under the lock: `edit` says whether it changed anything. `None` where
    /// the lock or the write failed.
    fn edit<T>(&self, edit: impl FnOnce(&mut Vec<Record>) -> (bool, T)) -> Option<T> {
        let _lock = self.lock()?;
        let mut records = self.kept();
        let (changed, out) = edit(&mut records);
        if changed {
            self.save(records).ok()?;
        }
        Some(out)
    }

    /// [`Store::observe`], its news returned instead of published.
    pub fn observe_with(&self, seen: &Observation, now: i64) -> (Option<Record>, Told) {
        let place_email = seen
            .seen()
            .map(|login| (login.path.clone(), login.email.clone()));
        let current = |records: &[Record]| {
            let (place, email) = place_email.as_ref()?;
            records
                .iter()
                .find(|record| record.is(place, email))
                .cloned()
        };
        // Nothing to write on most passes: decided on a copy, with no lock taken.
        {
            let mut copy = self.kept();
            if !observed(&mut copy, seen, now).0 {
                return (current(&copy), Told::default());
            }
        }
        let Some((records, learned)) = self.edit(|records| {
            let (changed, learned) = observed(records, seen, now);
            (changed, (records.clone(), learned))
        }) else {
            // Unwritten: what the file says stands, and the next pass tries again.
            return (current(&self.kept()), Told::default());
        };
        (current(&records), Told::of(self.cli, &learned, now))
    }

    /// Take what a login file says at `now` ([`super::observe::muse`]), telling the journal what
    /// it changes: the record of the login the file names (made at its first sighting), or none
    /// where it names no login.
    pub fn observe(&self, seen: &Observation, now: i64) -> Option<Record> {
        let (record, told) = self.observe_with(seen, now);
        told.publish();
        record
    }

    /// Leave to start one read of the login `place` names as `email`, at `now`. `asked` is a
    /// person's own ask. The admission is on record before the read runs, so no other window
    /// starts a second one.
    ///
    /// # Errors
    ///
    /// The login is not on record, is signed out, or was read too recently ([`Refusal`]).
    pub fn admit(&self, place: &Path, email: &str, asked: bool, now: i64) -> Result<Pass, Refusal> {
        self.edit(|records| {
            let Some(record) = records.iter_mut().find(|record| record.is(place, email)) else {
                return (false, Err(Refusal::Unlisted));
            };
            if let Err(refusal) = record.standing(asked || record.asked, now) {
                return (false, Err(refusal));
            }
            record.read_at = Some(now);
            record.asked = false;
            (
                true,
                Ok(Pass {
                    place: record.place.clone(),
                    email: record.email.clone(),
                    at: now,
                }),
            )
        })
        .unwrap_or(Err(Refusal::Unwritable))
    }

    /// [`Store::landed`], its news returned instead of published.
    pub fn landed_with(&self, pass: Pass, landing: Landing, now: i64) -> Told {
        let learned = self
            .edit(|records| {
                match records
                    .iter_mut()
                    .find(|record| record.is(&pass.place, &pass.email))
                {
                    Some(record) => (true, landed(record, landing, now)),
                    None => (false, Vec::new()),
                }
            })
            .unwrap_or_default();
        Told::of(self.cli, &learned, now)
    }

    /// Record what the read `pass` admitted said, at `now`, telling the journal: a reading
    /// (`auth` ok, its `quota`), the vendor's sign-in words (`signed-out`), or nothing (`error`).
    pub fn landed(&self, pass: Pass, landing: Landing, now: i64) {
        self.landed_with(pass, landing, now).publish();
    }

    /// [`Store::walled`], its news returned instead of published.
    pub fn walled_with(&self, seen: &Observation, now: i64) -> Told {
        let Some(login) = seen.seen() else {
            return Told::default();
        };
        let learned = self
            .edit(|records| {
                match records
                    .iter_mut()
                    .find(|record| record.is(&login.path, &login.email))
                    .filter(|record| record.auth != Auth::SignedOut)
                {
                    Some(record) => {
                        record.auth = Auth::SignedOut;
                        record.auth_at = Some(now);
                        record.asked = false;
                        (
                            true,
                            vec![Learned::SignedOut {
                                email: record.email.clone(),
                                src: "cli-output",
                            }],
                        )
                    }
                    None => (false, Vec::new()),
                }
            })
            .unwrap_or_default();
        Told::of(self.cli, &learned, now)
    }

    /// The CLI running on the login `seen` names stopped at its own sign-in wall at `now`: the
    /// login does not work, whatever its file says, so its record is signed out (source
    /// `cli-output`) and no background read starts on it until the file is written again (a new
    /// sign-in, [`Store::observe`]). A login not on record, or already signed out, is left.
    pub fn walled(&self, seen: &Observation, now: i64) {
        self.walled_with(seen, now).publish();
    }

    /// A person asked for a fresh read of every login of `email` (the fact menu's refresh): the
    /// next footer pass reads it once, whatever figure is on record, unless it is signed out.
    pub fn ask(&self, email: &str) {
        let _ = self.edit(|records| {
            let mut changed = false;
            for record in records
                .iter_mut()
                .filter(|record| record.email.eq_ignore_ascii_case(email.trim()))
            {
                changed |= !record.asked;
                record.asked = true;
            }
            (changed, ())
        });
    }
}

#[cfg(test)]
#[path = "observed_account_tests.rs"]
mod tests;
