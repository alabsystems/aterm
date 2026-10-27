// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keys the person changed after aterm set them (design §6.6).
//!
//! A key aterm set, or found as wanted, and read back so is `applied`. When an unattended pass
//! later reads it otherwise, the person chose that: the key is `kept`, and no unattended pass
//! (a retry, a widened plan, an update's successor, another Mac's profile, a legacy upgrade)
//! writes it again. Its row reads
//! [`aterm_types::macos_setup::CHANGED_BY_YOU`]. Restore, turning its group off and on, or
//! Set up now forgets it, and the next pass sets it.
//!
//! The record is a side file an older aterm never writes, not a field of the plan's records,
//! whose format rule would drop the field on an older aterm's rewrite (setting the Dock again)
//! or freeze setup. An older aterm may set a kept key again itself; nothing this build writes
//! can stop that, and the alternative is worse.

use super::*;

/// The record of the keys the person changed, beside the run's records in the state root.
pub(in crate::macos_doctor) const KEPT_RECORD: &str = "setup-kept.json";

/// The format this build writes in the record's `format`: a later one is a newer aterm's, which
/// this build reads what it keeps from and never writes.
const KEPT_FORMAT: u64 = 1;

/// Why the record cannot be read: an unattended pass then sets nothing.
const UNREADABLE: &str = "the record of the settings you changed is unreadable";

/// Why an unattended pass leaves a kept key as it is (the hold's words, which its row ends with).
pub(in crate::macos_doctor) const KEPT: &str = "Changed by you; aterm leaves it as you set it. Restore, turning its group off and on, or Set up now sets it again";

/// One key the record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::macos_doctor) struct Key {
    /// The item's id (`init.dock@<user>`).
    pub(in crate::macos_doctor) item: String,
    /// The System State row it was built for, where the pass said.
    pub(in crate::macos_doctor) setting: Option<Setting>,
    /// What it read when aterm set it or found it as wanted (the pass's rendering), where the
    /// pass said: a later reading of the same words is aterm's want moving, not the person.
    pub(in crate::macos_doctor) held: Option<String>,
}

/// The keys aterm set and the ones the person changed since.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::macos_doctor) struct Kept {
    /// Keys aterm set, or found as wanted, and read back so.
    pub(in crate::macos_doctor) applied: Vec<Key>,
    /// Keys that read otherwise after that: the person's.
    pub(in crate::macos_doctor) kept: Vec<Key>,
    /// A newer aterm wrote the record: its kept keys are honored, and it is never written.
    frozen: bool,
}

impl Kept {
    /// The record in `root`: none is empty.
    ///
    /// # Errors
    /// The record cannot be read, so which keys the person changed is unknown: an unattended
    /// pass then sets nothing rather than set one of them again.
    pub(in crate::macos_doctor) fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(KEPT_RECORD);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let record: Value = aterm_json::from_slice(&bytes).map_err(|_| UNREADABLE.to_owned())?;
        Self::parse(&record)
    }

    /// The record `record` holds.
    ///
    /// # Errors
    /// It is not a record of this profile's, or a field cannot be read.
    pub(in crate::macos_doctor) fn parse(record: &Value) -> Result<Self, String> {
        let bad = || UNREADABLE.to_owned();
        if record["profile"].as_str() != Some(PROFILE) {
            return Err(bad());
        }
        let frozen = match record.get("format").map(Value::as_u64) {
            Some(Some(format)) => format > KEPT_FORMAT,
            _ => return Err(bad()),
        };
        let keys = |field: &str| -> Result<Vec<Key>, String> {
            record[field]
                .as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|entry| {
                    let item = entry["item"].as_str().ok_or_else(bad)?.to_owned();
                    let setting = match &entry["setting"] {
                        Value::Null => None,
                        number => {
                            let number = number.as_u64().ok_or_else(bad)?;
                            Setting::ALL
                                .into_iter()
                                .find(|setting| *setting as u64 == number)
                                .ok_or_else(bad)
                                .map(Some)?
                        }
                    };
                    let held = match &entry["held"] {
                        Value::Null => None,
                        held => Some(held.as_str().ok_or_else(bad)?.to_owned()),
                    };
                    Ok(Key {
                        item,
                        setting,
                        held,
                    })
                })
                .collect()
        };
        Ok(Self {
            // A newer aterm's record is read for what it keeps only.
            applied: if frozen { Vec::new() } else { keys("applied")? },
            kept: keys("kept")?,
            frozen,
        })
    }

    /// The record as this build writes it.
    pub(in crate::macos_doctor) fn record(&self) -> Value {
        let keys = |keys: &[Key]| -> Vec<Value> {
            keys.iter()
                .map(|key| {
                    json!({
                        "item": key.item,
                        "setting": key.setting.map(|setting| setting as u64),
                        "held": key.held,
                    })
                })
                .collect()
        };
        json!({
            "profile": PROFILE,
            "format": KEPT_FORMAT,
            "applied": keys(&self.applied),
            "kept": keys(&self.kept),
        })
    }

    /// Save the record in `root`, unless a newer aterm wrote it.
    ///
    /// # Errors
    /// It cannot be written.
    pub(in crate::macos_doctor) fn save(&self, root: &Path) -> Result<(), String> {
        if self.frozen {
            return Ok(());
        }
        write_json(&root.join(KEPT_RECORD), &self.record())
    }

    /// What an unattended pass found: the items it `settled` (set and read back, found as
    /// wanted, or left as they were) with what each reads now where the pass said (`readings`),
    /// and, among them, the ones it left as they were although they read otherwise than aterm
    /// sets them (`drifted`); `setting_of` names an item's row. A drifted key that reads as
    /// aterm left it is aterm's want that moved (another build's), which the record lets go of;
    /// any other drifted key is the person's from now on, and a key read back as wanted is
    /// aterm's again. Whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "ApplyKey",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "PersonChanges",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(in crate::macos_doctor) fn note(
        &mut self,
        settled: &[String],
        readings: &[(String, String)],
        drifted: &[String],
        setting_of: &dyn Fn(&str) -> Option<Setting>,
    ) -> bool {
        let before = self.clone();
        let reading = |item: &str| {
            readings
                .iter()
                .find(|(each, _)| each == item)
                .map(|(_, reads)| reads.clone())
        };
        for item in drifted {
            let now = reading(item);
            let key = take(&mut self.applied, item).or_else(|| take(&mut self.kept, item));
            match key {
                Some(key) if now.is_some() && key.held == now && !before.keeps(item) => {}
                key => self.kept.push(key.unwrap_or_else(|| Key {
                    item: item.clone(),
                    setting: setting_of(item),
                    held: None,
                })),
            }
        }
        for item in settled.iter().filter(|item| !drifted.contains(item)) {
            let mut key = take(&mut self.kept, item)
                .or_else(|| take(&mut self.applied, item))
                .unwrap_or_else(|| Key {
                    item: item.clone(),
                    setting: setting_of(item),
                    held: None,
                });
            key.held = reading(item).or(key.held);
            self.applied.push(key);
        }
        *self != before
    }

    /// `holds`, an unattended pass's, with every key aterm applied left as it is should it read
    /// otherwise now (in `settled_words`' words for it, unless `holds` holds it already), and
    /// every kept key left as the person set it: in [`KEPT`]'s words, unless this run's own
    /// record leaves it as it is already, and in place of a failure's wait, which would end in a
    /// try.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "UnattendedPass",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(in crate::macos_doctor) fn hold(
        &self,
        holds: &mut Vec<(String, aterm_machines::init::Hold)>,
        settled_words: &dyn Fn(&str) -> &'static str,
    ) {
        use aterm_machines::init::Hold;
        for key in &self.applied {
            if !holds.iter().any(|(item, _)| *item == key.item) {
                holds.push((
                    key.item.clone(),
                    Hold::AsIs(settled_words(&key.item).into()),
                ));
            }
        }
        for key in &self.kept {
            match holds.iter_mut().find(|(item, _)| *item == key.item) {
                // This run's own record already leaves it as it is, in words that say the same.
                Some((_, Hold::AsIs(_))) => {}
                // A failure's wait would end in a try: the person's key is never tried again.
                Some((_, hold)) => *hold = Hold::AsIs(KEPT.into()),
                None => holds.push((key.item.clone(), Hold::AsIs(KEPT.into()))),
            }
        }
    }

    /// Restore, or Set up now: aterm sets every key again. Whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "Restore",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(in crate::macos_doctor) fn forget_all(&mut self) -> bool {
        let before = self.applied.len() + self.kept.len();
        self.applied.clear();
        self.kept.clear();
        before != 0
    }

    /// The groups of `settings` turned on again: aterm sets their keys again, and a key whose
    /// row is unknown too. Whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupLedger",
            action = "GroupOn",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project"
        )
    )]
    pub(in crate::macos_doctor) fn forget(&mut self, settings: Selection) -> bool {
        let before = self.applied.len() + self.kept.len();
        let stays = |key: &Key| {
            key.setting
                .is_some_and(|setting| !settings.contains(setting))
        };
        self.applied.retain(stays);
        self.kept.retain(stays);
        before != self.applied.len() + self.kept.len()
    }

    /// The rows whose keys the person changed.
    pub(in crate::macos_doctor) fn changed_by_you(&self) -> Selection {
        let mut settings = Selection::none();
        for setting in self.kept.iter().filter_map(|key| key.setting) {
            settings.set(setting, true);
        }
        settings
    }

    /// Whether the record keeps `item` as the person set it.
    pub(in crate::macos_doctor) fn keeps(&self, item: &str) -> bool {
        self.kept.iter().any(|key| key.item == item)
    }

    /// Whether the record has `item` as aterm set it. Read only by the tests that check the
    /// record's and the model's agreement.
    #[cfg(test)]
    pub(in crate::macos_doctor) fn applied(&self, item: &str) -> bool {
        self.applied.iter().any(|key| key.item == item)
    }
}

/// `item`'s key, taken out of `keys`.
fn take(keys: &mut Vec<Key>, item: &str) -> Option<Key> {
    let at = keys.iter().position(|key| key.item == item)?;
    Some(keys.remove(at))
}

/// Forget what the record keeps in `root`: everything (`None`: Restore, Set up now) or the
/// rows of `settings` (groups turned on again), so the next pass sets them. A record a newer
/// aterm wrote is left as it is; one that cannot be read is replaced, since what it kept is
/// forgotten either way.
///
/// # Errors
/// The record cannot be written.
pub(in crate::macos_doctor) fn forget_at(
    root: &Path,
    settings: Option<Selection>,
) -> Result<(), String> {
    let (mut kept, damaged) = match Kept::load(root) {
        Ok(kept) => (kept, false),
        Err(_) if settings.is_none() => (Kept::default(), true),
        Err(why) => return Err(why),
    };
    let changed = match settings {
        None => kept.forget_all(),
        Some(settings) => kept.forget(settings),
    };
    if changed || damaged {
        kept.save(root)?;
    }
    Ok(())
}

/// The rows whose keys the person changed, as the record in `root` says; none when it cannot be
/// read.
pub(in crate::macos_doctor) fn changed_by_you_at(root: &Path) -> Selection {
    Kept::load(root).map_or_else(|_| Selection::none(), |kept| kept.changed_by_you())
}
