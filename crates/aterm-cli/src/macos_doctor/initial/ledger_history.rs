// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional display values in the existing setup ledger; never setup authority.

use super::*;
use aterm_types::macos_setup::{Observation, Read, Standing, Subject};

const FIELD: &str = "display_history";
const FORMAT: u64 = 1;
const VALUE_BYTES: usize = 1024;
const HISTORY_BYTES: usize = 64 * 1024;

/// One useful value as it was displayed after its original read.
#[derive(Clone, PartialEq, Eq)]
pub struct LastRead {
    pub subject: Subject,
    pub standing: Standing,
    pub value: String,
    pub read_at_ms: u64,
}

impl std::fmt::Debug for LastRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LastRead")
            .field("subject", &self.subject)
            .field("read_at_ms", &self.read_at_ms)
            .finish_non_exhaustive()
    }
}

impl LastRead {
    /// Keep only a useful displayed value, never approval notes or native authority.
    #[must_use]
    pub fn capture(row: &Observation, read_at_ms: u64) -> Option<Self> {
        if matches!(row.standing, Standing::Unknown | Standing::Failed) || read_at_ms == 0 {
            return None;
        }
        let value = if row.standing == Standing::AtLogin {
            Some(&row.wanted)
        } else {
            row.now.as_ref()
        }?;
        if value.is_empty() || value.len() > VALUE_BYTES || value.contains('\0') {
            return None;
        }
        Some(Self {
            subject: row.subject,
            standing: row.standing,
            value: value.clone(),
            read_at_ms,
        })
    }

    /// A historical display row, never a current observation or an effect receipt.
    #[must_use]
    pub fn observation(&self) -> Observation {
        Observation {
            subject: self.subject,
            standing: self.standing,
            now: Some(self.value.clone()),
            wanted: self.value.clone(),
            after_login: None,
            note: None,
            detail: None,
            // Unstamped: evidence of no age, so a remembered value never verifies its row.
            read: Read::Unstamped,
        }
    }
}

/// A retained snapshot's non-authoritative file generation and display values.
#[derive(Clone)]
pub struct DisplayHistory {
    pub generation: u64,
    pub vault_source: String,
    pub rows: Vec<LastRead>,
}

/// Access to only the optional display section of this account's existing setup ledger.
#[derive(Clone)]
pub struct SetupHistoryStore {
    root: PathBuf,
    uid: u32,
    home: PathBuf,
}

impl SetupHistoryStore {
    /// The runtime's ordinary account; unresolved or refused scope grants no store.
    #[must_use]
    pub fn for_person() -> Option<Self> {
        if !matches!(person_scope(), PersonScope::Person) {
            return None;
        }
        Some(Self {
            root: state_root().ok()?,
            uid: aterm_uds::peer::our_uid(),
            home: aterm_types::dirs::home_dir()?,
        })
    }

    /// An owned fixture root, without consulting the person's profile.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn at(root: &Path, home: &Path) -> Self {
        Self {
            root: root.to_owned(),
            uid: aterm_uds::peer::our_uid(),
            home: home.to_owned(),
        }
    }

    fn read(&self) -> Option<Value> {
        match aterm_uds::private_file::read_owned(
            &self.root,
            LEDGER,
            aterm_uds::atomic_file::MAX_CONFIG_BYTES as usize,
        ) {
            Ok(bytes) => aterm_json::from_slice(&bytes).ok(),
            Err(aterm_uds::private_file::PrivateFileError::Missing) => Some(Ledger::of(0).record()),
            _ => None,
        }
    }

    /// Capture the generation before a native read. Unknown future or malformed history
    /// is not generation zero and cannot be overwritten by this build.
    #[must_use]
    pub fn begin(&self) -> Option<DisplayHistory> {
        let value = self.read()?;
        Ledger::parse(&value).ok()?;
        self.history(&value)
    }

    /// Saved display context before the GUI starts. Read no live facts, flush no records,
    /// and create no account identity. A partial recovery waits for the ordinary worker.
    #[must_use]
    pub fn display(&self) -> Option<(Plan, DisplayHistory)> {
        let mut history = self.begin()?;
        if history.rows.is_empty() {
            return None;
        }
        let read = |name| match aterm_uds::private_file::read_owned(
            &self.root,
            name,
            aterm_uds::atomic_file::MAX_CONFIG_BYTES as usize,
        ) {
            Ok(bytes) => Some(Some(bytes)),
            Err(aterm_uds::private_file::PrivateFileError::Missing) => Some(None),
            _ => None,
        };
        let names = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ];
        let snapshot = [read(names[0])?, read(names[1])?, read(names[2])?];
        for (name, before) in names.into_iter().zip(&snapshot) {
            if read(name)?.as_ref() != before.as_ref() {
                return None;
            }
        }
        let [plan_bytes, progress_bytes, decision_bytes] = snapshot;
        let plan_value = plan_bytes
            .map(|bytes| aterm_json::from_slice::<Value>(&bytes))
            .transpose()
            .ok()?;
        let plan = plan_value
            .as_ref()
            .map(parse_plan_record)
            .transpose()
            .ok()?;
        let progress = progress_bytes
            .map(|bytes| {
                let value = aterm_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())?;
                parse_progress(&value)
            })
            .transpose()
            .ok()?;
        if plan_value.as_ref().is_some_and(|value| {
            (value.get("upgrade").is_some() || scope::put_back(value))
                && progress.is_none_or(|progress| {
                    plan.as_ref()
                        .is_none_or(|(plan, _)| !progress.belongs_to(plan))
                })
        }) {
            return None;
        }
        let (decision, restored, cancelled) = decision_bytes
            .map(|bytes| parse_decision_reviewed(&bytes))
            .transpose()
            .ok()?
            .unwrap_or((Decision::Missing, None, None));
        let records = Records {
            plan,
            progress,
            decision,
            restored,
            cancelled,
            provisional: false,
            stop_request: None,
            person_access: false,
        };
        let configured =
            ConfiguredVault::read(&aterm_secrets::client::Paths::locate_for_home(&self.home));
        let source_read = configured.is_ok();
        let plan = offered_plan(records.presented_plan_record(), configured);
        if !source_read || history.vault_source.trim() != plan.vault_url.trim() {
            history.rows.retain(|row| !row.subject.uses_vault_source());
            history.vault_source.clear();
        }
        Some((plan, history))
    }

    fn history(&self, ledger: &Value) -> Option<DisplayHistory> {
        let Some(value) = ledger.get(FIELD) else {
            return Some(DisplayHistory {
                generation: 0,
                vault_source: String::new(),
                rows: Vec::new(),
            });
        };
        if aterm_json::to_vec(value).ok()?.len() > HISTORY_BYTES
            || value["format"].as_u64()? != FORMAT
            || value["uid"].as_u64()? != u64::from(self.uid)
            || Path::new(value["home"].as_str()?) != self.home
        {
            return None;
        }
        let generation = value["generation"].as_u64()?;
        if generation == 0 {
            return None;
        }
        let vault_source = value["vault_source"].as_str()?.to_owned();
        if vault_source.len() > VALUE_BYTES || vault_source.contains('\0') {
            return None;
        }
        let values = value["rows"].as_array()?;
        if values.len() > Subject::all().len() {
            return None;
        }
        let mut rows = Vec::new();
        for row in values {
            let subject = Subject::all()
                .into_iter()
                .find(|subject| Some(subject.setup_word()) == row["subject"].as_str())?;
            if rows
                .iter()
                .any(|previous: &LastRead| previous.subject == subject)
            {
                return None;
            }
            let standing = Standing::from_word(row["standing"].as_str()?)?;
            let observation = Observation {
                standing,
                now: Some(row["value"].as_str()?.to_owned()),
                wanted: row["value"].as_str()?.to_owned(),
                ..Observation::unknown(subject)
            };
            rows.push(LastRead::capture(
                &observation,
                row["read_at_ms"].as_u64()?,
            )?);
        }
        Some(DisplayHistory {
            generation,
            vault_source,
            rows,
        })
    }

    /// Save only an accepted snapshot whose pre-read generation still stands. A busy lock,
    /// another writer, unsafe metadata or unknown record skips this optional save. The
    /// callback runs under the existing file lock, releases its cache lock before I/O, and
    /// must not acquire another file lock. Setup effects never wait for display persistence.
    pub fn save(
        &self,
        basis: u64,
        vault_source: &str,
        rows: &[LastRead],
        current: impl FnOnce() -> bool,
    ) -> bool {
        let Some(home) = self.home.to_str() else {
            return false;
        };
        let Ok(Some(_lock)) = HeldLock::try_take_named(&self.root, LEDGER_LOCK) else {
            return false;
        };
        let Some(mut ledger) = self.read() else {
            return false;
        };
        if Ledger::parse(&ledger).is_err() {
            return false;
        }
        let Some(previous) = self.history(&ledger) else {
            return false;
        };
        let Some(generation) = basis.checked_add(1) else {
            return false;
        };
        if previous.generation != basis || !current() {
            return false;
        }
        let history = json!({
            "format": FORMAT, "generation": generation, "uid": self.uid,
            "home": home, "vault_source": vault_source,
            "rows": rows.iter().map(|row| json!({
                "subject": row.subject.setup_word(), "standing": row.standing.word(),
                "value": row.value, "read_at_ms": row.read_at_ms,
            })).collect::<Vec<_>>(),
        });
        ledger
            .as_object_mut()
            .expect("a parsed ledger is an object")
            .insert(FIELD.into(), history);
        if self.history(&ledger).is_none()
            || aterm_json::to_vec(&ledger).map_or(true, |bytes| {
                bytes.len() as u64 > aterm_uds::atomic_file::MAX_CONFIG_BYTES
            })
        {
            return false;
        }
        // Keep the CURRENT raw ledger, including its revision and unknown fields. Never
        // reconstruct authority from a snapshot captured before this lock.
        write_json(&self.root.join(LEDGER), &ledger).is_ok()
    }
}

pub(super) fn preserve(value: Option<&Value>, record: &mut Value) {
    if let Some(history) = value.and_then(|value| value.get(FIELD))
        && aterm_json::to_vec(history).is_ok_and(|bytes| bytes.len() <= HISTORY_BYTES)
    {
        record
            .as_object_mut()
            .expect("a ledger record is an object")
            .insert(FIELD.into(), history.clone());
        // Optional display data must not make the next authority read exceed its cap.
        if aterm_json::to_vec(record).map_or(true, |bytes| {
            bytes.len() as u64 > aterm_uds::atomic_file::MAX_CONFIG_BYTES
        }) {
            record
                .as_object_mut()
                .expect("a ledger record is an object")
                .remove(FIELD);
        }
    }
}
