// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keys that changed after aterm set them (design §6.6).
//!
//! A key aterm set, or found as wanted, and read back so is `applied`. When an unattended pass
//! later reads a preference otherwise, nothing tells who changed it: the person, another app, an
//! installer, macOS resetting a default. So the first such change is no evidence of a choice,
//! and the pass that reads it sets the key again, as it sets any key that is not as wanted, and
//! notes that it did (`again`); a try that does not take is tried again on the pass's own pause,
//! as any other. A change only to targets the read-back did not name (a keyboard model macOS
//! lists for the first time) is not a change of anything aterm set, and is made either way
//! ([`aterm_machines::items::Item::new_targets`]). Once aterm set a key again, a later change is
//! something that holds it otherwise: the key is `kept`, and no unattended pass (a retry, a
//! widened plan, an update's successor, another Mac's profile, a legacy upgrade) writes it
//! again. A sign-out is the exception: an account signed in again after aterm signed it out took
//! an Apple Account's sign-in, and signing it out once more would end that session, so its first
//! change is kept ([`SIGNED_IN`]). A kept key's row reads
//! [`aterm_types::macos_setup::CHANGED_SINCE`], which is not set up. Restore, turning its group
//! off and on, or Set up now forgets it, and the next pass sets it. A difference setup's own
//! step still owes (Notification Center's widget list waiting for Full Disk Access) is that
//! step's, never a change.
//!
//! The record is a side file an older aterm never writes, not a field of the plan's records,
//! whose format rule would drop the field on an older aterm's rewrite (setting the Dock again)
//! or freeze setup. An older aterm reads this build's format as a newer one's: it honors the
//! kept keys and writes nothing. This build reads the format before `again` (1), which kept a
//! preference on its first change, as having set none again: such a key is aterm's to set once
//! more ([`Kept::parse`]).

use super::*;

/// The record of the keys that changed since aterm set them, beside the run's records in the
/// state root.
pub(in crate::macos_doctor) const KEPT_RECORD: &str = "setup-kept.json";

/// The format this build writes in the record's `format`: a later one is a newer aterm's, which
/// this build reads what it keeps from and never writes. Format 1 had no `again`, and reads as
/// no key set again ([`Kept::parse`]).
const KEPT_FORMAT: u64 = 2;

/// Why the record cannot be read: an unattended pass then sets nothing.
const UNREADABLE: &str = "the record of the settings changed since aterm set them is unreadable";

/// Why an unattended pass leaves a kept preference as it is (the hold's words, which its row ends
/// with).
pub(in crate::macos_doctor) const KEPT: &str = "Changed since aterm set it, and again after aterm set it once more; aterm leaves it as it is. Restore, turning its group off and on, or Set up now sets it again";

/// Why an unattended pass leaves a kept sign-out as it is: the account was signed in again since
/// aterm signed it out, which takes the person's Apple Account.
pub(in crate::macos_doctor) const SIGNED_IN: &str = "Signed in again since aterm signed it out, which takes a person's Apple Account; aterm leaves it as it is. Restore, turning its group off and on, or Set up now signs it out again";

/// One key the record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::macos_doctor) struct Key {
    /// The item's id (`init.dock@<user>`).
    pub(in crate::macos_doctor) item: String,
    /// The System State row it was built for, where the pass said.
    pub(in crate::macos_doctor) setting: Option<Setting>,
    /// What it read when aterm set it or found it as wanted (the pass's rendering), where the
    /// pass said: a later reading of the same words is aterm's want moving, not a change.
    pub(in crate::macos_doctor) held: Option<String>,
    /// Whether aterm set it again after it changed since it first read back as wanted: a later
    /// change keeps it.
    pub(in crate::macos_doctor) again: bool,
}

impl Key {
    /// Whether it is a sign-out (Messages, FaceTime), whose first change is kept.
    fn signs_out(&self) -> bool {
        self.setting
            .is_some_and(|setting| setting.group() == aterm_types::macos_setup::Group::Accounts)
    }

    /// The words an unattended pass leaves it as it is in, once it is kept.
    fn kept_words(&self) -> &'static str {
        if self.signs_out() { SIGNED_IN } else { KEPT }
    }
}

/// What an unattended pass found of the keys it reached ([`Kept::note`]).
pub(in crate::macos_doctor) struct Found<'a> {
    /// The items it settled: set and read back, found as wanted, or left as they were.
    pub(in crate::macos_doctor) settled: &'a [String],
    /// What each settled item reads now, where the pass said.
    pub(in crate::macos_doctor) readings: &'a [(String, String)],
    /// Of `settled`, the items it left as they were although they read otherwise than aterm sets
    /// them.
    pub(in crate::macos_doctor) drifted: &'a [String],
    /// Of `settled`, the items it changed and read back as wanted, each with what it read
    /// before the change.
    pub(in crate::macos_doctor) changed: &'a [(String, String)],
    /// Of `changed`, the items whose change was only to targets their read-back did not name.
    pub(in crate::macos_doctor) new_targets: &'a [String],
    /// Whether an item's difference is a step setup still owes it (Notification Center's widget
    /// list, waiting for Full Disk Access): that step's, never a change.
    pub(in crate::macos_doctor) owed: &'a dyn Fn(&str) -> bool,
}

/// The keys aterm set and the ones that changed since.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::macos_doctor) struct Kept {
    /// Keys aterm set, or found as wanted, and read back so.
    pub(in crate::macos_doctor) applied: Vec<Key>,
    /// Keys that read otherwise once more after aterm set them again, or a sign-out signed in
    /// again after aterm did it, by whoever changed them: left as they are.
    pub(in crate::macos_doctor) kept: Vec<Key>,
    /// A newer aterm wrote the record (a later `format`, or a row this build does not know):
    /// its kept keys are honored, and it is never written.
    frozen: bool,
}

impl Kept {
    /// The record in `root`: none is empty.
    ///
    /// # Errors
    /// The record cannot be read, so which keys changed since aterm set them is unknown: an
    /// unattended pass then sets nothing rather than set one of them again.
    pub(in crate::macos_doctor) fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(KEPT_RECORD);
        let bytes = match crate::macos_doctor::read_record_bytes(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let record: Value = aterm_json::from_slice(&bytes).map_err(|_| UNREADABLE.to_owned())?;
        Self::parse(&record)
    }

    /// The record `record` holds. One a newer aterm wrote (a later `format`, or a row this build
    /// does not know, which only a newer aterm names) is frozen: a key on a row this build does
    /// not know is kept with no row, so it is still left as it is, and the rest is
    /// honored. One of the format before `again` (1) kept a preference on its first change, which
    /// is no evidence of a choice: each such key reads as aterm's, set and read back, never set
    /// again, so the next pass that finds it otherwise sets it once more; its sign-outs stay kept.
    ///
    /// # Errors
    /// It is not a record of this profile's, or a field cannot be read.
    pub(in crate::macos_doctor) fn parse(record: &Value) -> Result<Self, String> {
        let bad = || UNREADABLE.to_owned();
        if record["profile"].as_str() != Some(PROFILE) {
            return Err(bad());
        }
        let known = |number: u64| {
            Setting::ALL
                .into_iter()
                .find(|setting| *setting as u64 == number)
        };
        let new_row = ["applied", "kept"].iter().any(|field| {
            record[*field].as_array().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["setting"]
                        .as_u64()
                        .is_some_and(|number| known(number).is_none())
                })
            })
        });
        let (frozen, before_again) = match record.get("format").map(Value::as_u64) {
            Some(Some(format)) => (format > KEPT_FORMAT || new_row, format < KEPT_FORMAT),
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
                        number => known(number.as_u64().ok_or_else(bad)?),
                    };
                    let held = match &entry["held"] {
                        Value::Null => None,
                        held => Some(held.as_str().ok_or_else(bad)?.to_owned()),
                    };
                    let again = match &entry["again"] {
                        Value::Null => false,
                        again => again.as_bool().ok_or_else(bad)?,
                    };
                    Ok(Key {
                        item,
                        setting,
                        held,
                        again,
                    })
                })
                .collect()
        };
        // A newer aterm's record is read for what it keeps only.
        let mut applied = if frozen { Vec::new() } else { keys("applied")? };
        let mut kept = keys("kept")?;
        if before_again && !frozen {
            let (signs_out, preferences): (Vec<Key>, Vec<Key>) =
                kept.into_iter().partition(Key::signs_out);
            kept = signs_out;
            applied.extend(preferences);
        }
        Ok(Self {
            applied,
            kept,
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
                        "again": key.again,
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

    /// What an unattended pass `found` ([`Found`]); `setting_of` names an item's row. A key that
    /// reads as wanted is aterm's: a change the pass made to it, from what it read back, is noted
    /// as setting it again, unless the change was only to new targets, or it read as aterm left
    /// it (aterm's want that moved, another build's). A key the pass left as it was although it
    /// reads otherwise ([`Self::hold`]: a preference aterm set again already, a sign-out signed in
    /// again, or one kept) is kept from now on, unless it reads as aterm left it, which is
    /// aterm's want that moved and lets the record go of it, or setup still owes its step.
    /// Whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "ApplyKey",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "ApplyAgain",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "KeepChanged",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "UnattendedPass",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    pub(in crate::macos_doctor) fn note(
        &mut self,
        found: &Found<'_>,
        setting_of: &dyn Fn(&str) -> Option<Setting>,
    ) -> bool {
        let before = self.clone();
        let reading = |item: &str| {
            found
                .readings
                .iter()
                .find(|(each, _)| each == item)
                .map(|(_, reads)| reads.clone())
        };
        let fresh = |item: &String| Key {
            item: item.clone(),
            setting: setting_of(item),
            held: None,
            again: false,
        };
        for item in found.drifted {
            if (found.owed)(item) {
                continue;
            }
            let now = reading(item);
            let key = take(&mut self.applied, item).or_else(|| take(&mut self.kept, item));
            match key {
                Some(key) if now.is_some() && key.held == now && !before.keeps(item) => {}
                key => self.kept.push(key.unwrap_or_else(|| fresh(item))),
            }
        }
        for item in found
            .settled
            .iter()
            .filter(|item| !found.drifted.contains(item))
        {
            let applied = before.applied(item);
            let mut key = take(&mut self.kept, item)
                .or_else(|| take(&mut self.applied, item))
                .unwrap_or_else(|| fresh(item));
            if let Some((_, from)) = found.changed.iter().find(|(each, _)| each == item)
                && !found.new_targets.contains(item)
            {
                // From what it read back: set again, unless aterm left it so (its want moved).
                key.again = applied && key.held.as_ref() != Some(from);
            }
            key.held = reading(item).or(key.held);
            self.applied.push(key);
        }
        *self != before
    }

    /// `holds`, an unattended pass's, with every preference aterm applied held as read back
    /// ([`aterm_machines::init::Hold::ReadBack`]): a change made once more unless aterm set it
    /// again already, then left as it is in [`KEPT`]'s words. A sign-out aterm did is left as it
    /// is, in [`SIGNED_IN`]'s words, and one a step setup still owes in `owed`'s words for it.
    /// `holds` own hold of a key stands, and every kept key is left as it is: in its words,
    /// unless this run's own record leaves it as it is already, and in place of a failure's wait,
    /// which would end in a try.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "UnattendedPass",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "ApplyAgain",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "KeepChanged",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
        )
    )]
    pub(in crate::macos_doctor) fn hold(
        &self,
        holds: &mut Vec<(String, aterm_machines::init::Hold)>,
        owed: &dyn Fn(&str) -> Option<&'static str>,
    ) {
        use aterm_machines::init::Hold;
        if super::ledger::planted(super::ledger::Defect::HoldForgetsKept) {
            return;
        }
        for key in &self.applied {
            if holds.iter().any(|(item, _)| *item == key.item) {
                continue;
            }
            let hold = match owed(&key.item) {
                Some(words) => Hold::AsIs(words.into()),
                None if key.signs_out() => Hold::AsIs(SIGNED_IN.into()),
                // The defect: the first change after a read-back is kept, with no try again.
                None if super::ledger::planted(super::ledger::Defect::KeepsFirstChange) => {
                    Hold::AsIs(KEPT.into())
                }
                None => Hold::ReadBack {
                    held: key.held.clone(),
                    again: key.again,
                    why: KEPT.into(),
                },
            };
            holds.push((key.item.clone(), hold));
        }
        for key in &self.kept {
            let words = key.kept_words();
            match holds.iter_mut().find(|(item, _)| *item == key.item) {
                // This run's own record already leaves it as it is.
                Some((_, Hold::AsIs(_))) => {}
                // A failure's wait would end in a try: a kept key is never tried again.
                Some((_, hold)) => *hold = Hold::AsIs(words.into()),
                None => holds.push((key.item.clone(), Hold::AsIs(words.into()))),
            }
        }
    }

    /// Each kept key left as it is, in its words: what a check holds, so it reads the key
    /// `changed`, as the unattended pass leaves it, never as drift ([`Self::hold`]).
    pub(in crate::macos_doctor) fn kept_holds(&self) -> Vec<(String, aterm_machines::init::Hold)> {
        self.kept
            .iter()
            .map(|key| {
                (
                    key.item.clone(),
                    aterm_machines::init::Hold::AsIs(key.kept_words().into()),
                )
            })
            .collect()
    }

    /// Restore, or Set up now: aterm sets every key again. Whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SetupKeptKey",
            action = "Restore",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
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
            machine = "SetupKeptKey",
            action = "GroupOn",
            project = "aterm_cli::macos_doctor::initial::tests::ledger_conformance::project_key"
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

    /// The rows whose keys changed since aterm set them.
    pub(in crate::macos_doctor) fn changed_since(&self) -> Selection {
        let mut settings = Selection::none();
        for setting in self.kept.iter().filter_map(|key| key.setting) {
            settings.set(setting, true);
        }
        settings
    }

    /// Whether the record keeps `item` as it is, changed since aterm set it.
    pub(in crate::macos_doctor) fn keeps(&self, item: &str) -> bool {
        self.kept.iter().any(|key| key.item == item)
    }

    /// Whether the record has `item` as aterm set it.
    pub(in crate::macos_doctor) fn applied(&self, item: &str) -> bool {
        self.applied.iter().any(|key| key.item == item)
    }

    /// Whether the record has `item` as aterm set it again after it changed. Read only by the
    /// tests that check the record's and the model's agreement.
    #[cfg(test)]
    pub(in crate::macos_doctor) fn set_again(&self, item: &str) -> bool {
        self.applied
            .iter()
            .chain(&self.kept)
            .any(|key| key.item == item && key.again)
    }

    /// Whether a newer aterm wrote the record: this build reads what it keeps and writes nothing,
    /// so nothing it sets again is noted.
    pub(in crate::macos_doctor) const fn frozen(&self) -> bool {
        self.frozen
    }
}

/// `item`'s key, taken out of `keys`.
fn take(keys: &mut Vec<Key>, item: &str) -> Option<Key> {
    let at = keys.iter().position(|key| key.item == item)?;
    Some(keys.remove(at))
}

/// Forget what the record keeps in `root`: everything (`None`: Restore, Set up now) or the
/// rows of `settings` (groups turned on again), so the next pass sets them. A record a newer
/// aterm wrote is left as it is, read or not; one of this build's format that cannot be read is
/// replaced, since what it kept is forgotten either way.
///
/// # Errors
/// The record cannot be written.
pub(in crate::macos_doctor) fn forget_at(
    root: &Path,
    settings: Option<Selection>,
) -> Result<(), String> {
    let (mut kept, damaged) = match Kept::load(root) {
        Ok(kept) => (kept, false),
        Err(_) if newer_at(root) => return Ok(()),
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

/// Whether the record in `root` names a later `format` than this build writes, whether or not
/// the rest of it reads: a newer aterm's, which this build never writes over.
fn newer_at(root: &Path) -> bool {
    crate::macos_doctor::read_record_bytes(&root.join(KEPT_RECORD))
        .ok()
        .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|record| {
            record["format"]
                .as_u64()
                .is_some_and(|format| format > KEPT_FORMAT)
        })
}

/// The rows whose keys changed since aterm set them, as the record in `root` says; none when it
/// cannot be read.
pub(in crate::macos_doctor) fn changed_since_at(root: &Path) -> Selection {
    Kept::load(root).map_or_else(|_| Selection::none(), |kept| kept.changed_since())
}
