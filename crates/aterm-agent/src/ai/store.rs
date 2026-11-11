// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local AI login state store under `<aterm state>/ai/`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Number, Value};

/// The directory under the aterm state root.
pub const DIR: &str = "ai";
/// The record's file name.
pub const RECORD_FILE: &str = "connections.json";
/// The lock's file name.
pub const LOCK_FILE: &str = "ai.lock";
/// The wake marker's file name.
pub const WAKE_FILE: &str = "wake";
/// The record's schema.
pub const SCHEMA: i64 = 2;
/// The largest record read.
pub const RECORD_MAX_BYTES: u64 = 64 * 1024;

/// How a login was last used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A review aterm ran.
    Review,
    /// A sign-in that finished, read by the row's status read.
    Login,
}

impl How {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            How::Review => "review",
            How::Login => "login",
        }
    }

    /// The word read back.
    #[must_use]
    pub fn parse(word: &str) -> Option<How> {
        [How::Review, How::Login]
            .into_iter()
            .find(|h| h.word() == word)
    }
}

/// Why a login reads ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// A review's error output said the login has ended.
    ReviewAuth,
    /// The status read after a sign-in said nobody is signed in.
    StatusSignedOut,
}

impl Ended {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Ended::ReviewAuth => "review-auth",
            Ended::StatusSignedOut => "status-signed-out",
        }
    }

    /// The word read back.
    #[must_use]
    pub fn parse(word: &str) -> Option<Ended> {
        [Ended::ReviewAuth, Ended::StatusSignedOut]
            .into_iter()
            .find(|e| e.word() == word)
    }

    /// In a person's words.
    #[must_use]
    pub const fn says(self) -> &'static str {
        match self {
            Ended::ReviewAuth => "a review could not authenticate",
            Ended::StatusSignedOut => "its status says nobody is signed in",
        }
    }
}

/// What one run showed of a login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It worked.
    Worked,
    /// The login has ended.
    Ended(Ended),
    /// The usage limit is reached, until this time when the text named it.
    Limited(Option<i64>),
}

fn int(n: i64) -> Value {
    Value::Number(Number::from(n))
}

/// One provider login's health record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// Opaque observation identity, changed by every stored note.
    pub generation: Option<String>,
    /// The newest run that worked, unix seconds.
    pub used_at: Option<i64>,
    /// How.
    pub used_how: Option<How>,
    /// The newest end seen, unix seconds.
    pub ended_at: Option<i64>,
    /// Why it ended.
    pub ended_why: Option<Ended>,
    /// When the usage limit lifts, unix seconds.
    pub limited_until: Option<i64>,
}

impl Record {
    /// Applies `outcome`, seen at `now` by `how`.
    pub fn apply(&mut self, how: How, outcome: Outcome, now: i64) {
        match outcome {
            Outcome::Worked => {
                self.used_at = Some(self.used_at.map_or(now, |t| t.max(now)));
                self.used_how = Some(how);
                self.limited_until = self.limited_until.filter(|t| *t > now);
            }
            Outcome::Ended(why) => {
                self.ended_at = Some(now);
                self.ended_why = Some(why);
            }
            Outcome::Limited(until) => {
                self.used_at = Some(self.used_at.map_or(now, |t| t.max(now)));
                self.used_how = Some(how);
                if let Some(t) = until {
                    self.limited_until = Some(t);
                }
            }
        }
    }

    /// Whether the newest thing seen of this login is its end.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.ended_at
            .is_some_and(|end| self.used_at.is_none_or(|used| end >= used))
    }

    fn to_json(&self) -> Value {
        let mut o = Map::new();
        if let Some(generation) = &self.generation {
            o.insert("generation".to_string(), Value::from(generation.clone()));
        }
        if let Some(t) = self.used_at {
            o.insert("used_at".to_string(), int(t));
        }
        if let Some(h) = self.used_how {
            o.insert("used_how".to_string(), Value::from(h.word()));
        }
        if let Some(t) = self.ended_at {
            o.insert("ended_at".to_string(), int(t));
        }
        if let Some(e) = self.ended_why {
            o.insert("ended_why".to_string(), Value::from(e.word()));
        }
        if let Some(t) = self.limited_until {
            o.insert("limited_until".to_string(), int(t));
        }
        Value::Object(o)
    }

    fn of_json(v: &Value) -> Result<Record, Unread> {
        if !v.is_object() {
            return Err(Unread::Torn("invalid health record".into()));
        }
        let int = |k: &str| v.get(k).and_then(Value::as_i64);
        let word = |k: &str| v.get(k).and_then(Value::as_str);
        let generation = match v.get("generation") {
            None => None,
            Some(value) => {
                let Some(s) = value
                    .as_str()
                    .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                else {
                    return Err(Unread::Torn("invalid observation generation".to_string()));
                };
                Some(s.to_string())
            }
        };
        Ok(Record {
            generation,
            used_at: int("used_at"),
            used_how: word("used_how").and_then(How::parse),
            ended_at: int("ended_at"),
            ended_why: word("ended_why").and_then(Ended::parse),
            limited_until: int("limited_until"),
        })
    }
}

/// Why a record file was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// It is not JSON this build reads, or not a regular file.
    Torn(String),
    /// Another schema: not used, never written over.
    NotOurs(i64),
}

/// Provider health records, separated by login scope where the provider supports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Records {
    /// By connection id.
    pub by_id: BTreeMap<String, Record>,
    /// Provider-local login scopes. Legacy records stay separate and are never guessed.
    pub by_scope: BTreeMap<String, BTreeMap<String, Record>>,
}

impl Records {
    /// Parses a record file's text.
    /// # Errors
    /// Why it is not used ([`Unread`]).
    pub fn parse(text: &str) -> Result<Records, Unread> {
        let doc: Value = aterm_json::from_str(text).map_err(|e| Unread::Torn(e.to_string()))?;
        match doc.get("schema").and_then(Value::as_i64) {
            Some(1 | SCHEMA) => {}
            Some(n) => return Err(Unread::NotOurs(n)),
            None => return Err(Unread::Torn("no schema".to_string())),
        }
        let mut by_id = BTreeMap::new();
        if let Some(value) = doc.get("clis") {
            let rows = value
                .as_object()
                .ok_or_else(|| Unread::Torn("invalid legacy health records".into()))?;
            for (id, row) in rows {
                if super::connector::connector(id).is_some() {
                    by_id.insert(id.clone(), Record::of_json(row)?);
                }
            }
        }
        let mut by_scope = BTreeMap::new();
        if let Some(value) = doc.get("scopes") {
            let providers = value
                .as_object()
                .ok_or_else(|| Unread::Torn("invalid health scopes".into()))?;
            for (id, value) in providers {
                let scopes = value
                    .as_object()
                    .ok_or_else(|| Unread::Torn("invalid provider health scopes".into()))?;
                let mut records = BTreeMap::new();
                for (scope, record) in scopes {
                    records.insert(scope.clone(), Record::of_json(record)?);
                }
                by_scope.insert(id.clone(), records);
            }
        }
        Ok(Records { by_id, by_scope })
    }

    /// The text written.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rows = Map::new();
        for (id, r) in &self.by_id {
            rows.insert(id.clone(), r.to_json());
        }
        let mut o = Map::new();
        o.insert("schema".to_string(), int(SCHEMA));
        o.insert("clis".to_string(), Value::Object(rows));
        let mut providers = Map::new();
        for (id, scopes) in &self.by_scope {
            let mut records = Map::new();
            for (scope, record) in scopes {
                records.insert(scope.clone(), record.to_json());
            }
            providers.insert(id.clone(), Value::Object(records));
        }
        o.insert("scopes".into(), Value::Object(providers));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// `id`'s record, or an empty one.
    #[must_use]
    pub fn of(&self, id: &str) -> Record {
        self.of_scope(id, None)
    }

    /// A login scope's own record, with no fallback to a different or legacy scope.
    #[must_use]
    pub fn of_scope(&self, id: &str, scope: Option<&str>) -> Record {
        match scope {
            Some(scope) => self.by_scope.get(id).and_then(|records| records.get(scope)),
            None => self.by_id.get(id),
        }
        .cloned()
        .unwrap_or_default()
    }
}

/// The held lock; dropping it releases it.
#[derive(Debug)]
pub struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// `<aterm state>/ai/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store under the aterm state root.
    #[must_use]
    pub fn at(aterm_state: &Path) -> Store {
        Store {
            dir: aterm_state.join(DIR),
        }
    }

    /// Its directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The wake marker the window watches.
    #[must_use]
    pub fn wake_path(&self) -> PathBuf {
        self.dir.join(WAKE_FILE)
    }

    /// Makes the directory, 0700, this user's own.
    /// # Errors
    /// It cannot be made, or it is not this user's private directory.
    pub fn ensure(&self) -> io::Result<()> {
        aterm_types::fs_restricted::ensure_private_dir(&self.dir)
    }

    /// Takes `ai.lock`, waiting for its holder.
    /// # Errors
    /// The directory or the lock file cannot be made or opened.
    pub fn acquire(&self) -> io::Result<Lock> {
        self.ensure()?;
        let mut open = std::fs::OpenOptions::new();
        open.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file: std::fs::File = aterm_uds::kept_out(|| open.open(self.dir.join(LOCK_FILE)))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ai.lock is not a regular file",
            ));
        }
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by unlock"
            )]
            let taken = file.lock();
            match taken {
                Ok(()) => return Ok(Lock(file)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The records; an absent file is empty.
    /// # Errors
    /// Why the file is not used ([`Unread`]).
    pub fn load(&self) -> Result<Records, Unread> {
        match crate::harness::accounts::read_bounded(&self.dir.join(RECORD_FILE), RECORD_MAX_BYTES)
        {
            Ok(None) => Ok(Records::default()),
            Ok(Some(text)) => Records::parse(&text),
            Err(e) => Err(Unread::Torn(e.to_string())),
        }
    }

    /// Records `outcome` of `id`, seen by `how` at `now`, then rewrites the wake.
    /// # Errors
    /// The lock or record cannot be read, or the record or wake cannot be written.
    pub fn note(&self, id: &str, how: How, outcome: Outcome, now: i64) -> io::Result<()> {
        self.note_scope(id, None, how, outcome, now)
    }

    /// Record an outcome in one provider-local login scope, preserving every other scope.
    /// # Errors
    /// The lock or record cannot be read, or the record or wake cannot be written.
    pub fn note_scope(
        &self,
        id: &str,
        scope: Option<&str>,
        how: How,
        outcome: Outcome,
        now: i64,
    ) -> io::Result<()> {
        let _held = self.acquire()?;
        let mut records = match self.load() {
            Ok(records) => records,
            Err(Unread::Torn(why)) => return Err(io::Error::new(io::ErrorKind::InvalidData, why)),
            Err(Unread::NotOurs(_)) => return Ok(()),
        };
        self.write_note(&mut records, id, scope, how, outcome, now)
    }

    /// Records a run's outcome only while its frozen login observation is still current.
    /// # Errors
    /// The lock, entropy, record or wake could not be read or written.
    pub fn note_if_current(
        &self,
        id: &str,
        expected: &Record,
        how: How,
        outcome: Outcome,
        now: i64,
    ) -> io::Result<bool> {
        self.note_if_current_scope(id, None, expected, how, outcome, now)
    }

    /// Complete an observation only while the same provider-local scope is current.
    /// # Errors
    /// The lock, entropy, record or wake could not be read or written.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ScopedHealth",
            action = "Observe",
            reason = "Choose the provider and scope whose outcome is in flight."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ScopedHealth",
            action = "Change",
            reason = "Choose the independently updated provider and scope."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ScopedHealth",
            action = "Outcome",
            reason = "Choose the completion outcome fixture."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ScopedHealth",
            action = "Commit",
            project = "aterm_agent::ai::store::tests::scoped_health_transition"
        )
    )]
    pub fn note_if_current_scope(
        &self,
        id: &str,
        scope: Option<&str>,
        expected: &Record,
        how: How,
        outcome: Outcome,
        now: i64,
    ) -> io::Result<bool> {
        let _held = self.acquire()?;
        let Ok(mut records) = self.load() else {
            return Ok(false);
        };
        if records.of_scope(id, scope) != *expected {
            return Ok(false);
        }
        self.write_note(&mut records, id, scope, how, outcome, now)?;
        Ok(true)
    }

    fn write_note(
        &self,
        records: &mut Records,
        id: &str,
        scope: Option<&str>,
        how: How,
        outcome: Outcome,
        now: i64,
    ) -> io::Result<()> {
        let generation = aterm_uds::rand::hex_token::<16>()?;
        let record = match scope {
            Some(scope) => records
                .by_scope
                .entry(id.to_owned())
                .or_default()
                .entry(scope.to_owned())
                .or_default(),
            None => records.by_id.entry(id.to_owned()).or_default(),
        };
        record.apply(how, outcome, now);
        record.generation = Some(generation);
        let text = records.render();
        if text.len() as u64 > RECORD_MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "AI health records exceed the read limit",
            ));
        }
        crate::harness::atomic_write::write_atomic_ordered(&self.dir.join(RECORD_FILE), &text)?;
        crate::harness::atomic_write::write_atomic_ordered(&self.wake_path(), "")
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
