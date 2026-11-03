// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Intrinsic archive relationships use the STORED reference, before comparing a request.
//! This reads only retained observations; it neither covers metadata nor repairs history.

use super::*;

pub(super) struct Envelope {
    pub(super) reference: CohortClosureRef,
    pub(super) journal: Journal,
    pub(super) history: history::History,
    pub(super) compaction: Option<Compaction>,
}

/// The matching original file remains held on the foreign no-cover observation path.
pub(super) struct Compaction {
    file: File,
    text: String,
    stamp: Stamp,
    value: Value,
    receipt: Value,
}

/// Only a successful covering operation permits consuming an existing deletion intent.
pub(super) struct CoveredCompaction(Compaction);

impl Compaction {
    fn inspect(
        slot: &slot::Inspection,
        receipt: &Value,
        receipt_stamp: &Stamp,
    ) -> io::Result<Option<Self>> {
        let (file, text, stamp) = match slot.read(COMPACTED) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let value =
            aterm_json::from_str(&text).map_err(|_| bad("torn terminal compaction receipt"))?;
        compaction(&value, receipt, receipt_stamp, slot.identity())?;
        Ok(Some(Self {
            file,
            text,
            stamp,
            value,
            receipt: receipt.clone(),
        }))
    }

    pub(super) fn publish(
        slot: &mut Slot,
        value: &Value,
        receipt: &Value,
        receipt_stamp: &Stamp,
    ) -> io::Result<CoveredCompaction> {
        compaction(value, receipt, receipt_stamp, slot.identity())?;
        let text =
            aterm_json::to_string(value).map_err(|_| bad("cannot encode terminal compaction"))?;
        let (file, stamp) = slot.sealed(COMPACTED, &text)?;
        Ok(CoveredCompaction(Self {
            file,
            text,
            stamp,
            value: value.clone(),
            receipt: receipt.clone(),
        }))
    }

    pub(super) fn require_receipt(&self, receipt: &Value) -> io::Result<()> {
        if self.receipt != *receipt {
            return Err(bad(
                "compaction intent belongs to a different original receipt",
            ));
        }
        Ok(())
    }

    fn recheck(&self, slot: &Slot) -> io::Result<()> {
        slot.check()?;
        if self.receipt.get("slot") != Some(slot.identity())
            || self.receipt.get("slot_gate") != Some(&slot.gate_stamp().value())
            || Stamp::read(&self.file)? != self.stamp
            || Stamp::read(&open(slot.file(), COMPACTED, false, false)?)? != self.stamp
        {
            return Err(bad(
                "compaction intent changed from its matched original file",
            ));
        }
        Ok(())
    }

    pub(super) fn cover(self, slot: &Slot) -> io::Result<CoveredCompaction> {
        self.recheck(slot)?;
        slot.cover(COMPACTED, &self.file, &self.text, &self.stamp)?;
        self.recheck(slot)?;
        Ok(CoveredCompaction(self))
    }
}

impl CoveredCompaction {
    pub(super) fn value(&self) -> &Value {
        &self.0.value
    }
    pub(super) fn require(&self, slot: &Slot, receipt: &Value) -> io::Result<()> {
        self.0.require_receipt(receipt)?;
        self.0.recheck(slot)
    }
}

fn field<'a>(value: &'a Value, name: &str) -> io::Result<&'a Value> {
    value
        .get(name)
        .ok_or_else(|| bad("archive envelope lacks a required field"))
}

/// The physical identity encoding emitted by identity(): dev/inode/mode/owner, never null.
pub(super) fn physical(value: &Value, directory: bool) -> io::Result<()> {
    let values = value
        .as_array()
        .filter(|v| v.len() == 4)
        .ok_or_else(|| bad("archive envelope lacks a typed physical identity"))?;
    if values.iter().any(|v| v.as_u64().is_none()) {
        return Err(bad("archive identity contains a non-unsigned field"));
    }
    let mode = u32::try_from(values[2].as_u64().unwrap())
        .map_err(|_| bad("archive identity mode is out of range"))?;
    let kind = fd::FileType::from_raw_mode(mode);
    // SAFETY: geteuid reads the current effective identity without pointers.
    let uid = unsafe { libc::geteuid() };
    if (directory && !kind.is_dir())
        || (!directory && !kind.is_file())
        || mode & 0o077 != 0
        || values[3].as_u64() != Some(u64::from(uid))
    {
        return Err(bad(
            "archive identity is not a private owned physical entry",
        ));
    }
    Ok(())
}

pub(super) fn origin(value: &Value) -> io::Result<Stamp> {
    let stamp = Stamp::decode(value)?;
    let values = &stamp.0;
    let mode = values[3]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| bad("archive origin has an invalid mode"))?;
    // SAFETY: geteuid reads the current effective identity without pointers.
    let uid = unsafe { libc::geteuid() };
    if values[..6].iter().any(|v| v.as_u64().is_none())
        || !fd::FileType::from_raw_mode(mode).is_file()
        || mode & 0o177 != 0
        || values[4].as_u64() != Some(1)
        || values[5].as_u64() != Some(u64::from(uid))
    {
        return Err(bad(
            "archive origin is not a private owned regular-file stamp",
        ));
    }
    Ok(stamp)
}

pub(super) struct FiniteInventory {
    pub(super) directories: std::collections::BTreeMap<String, Value>,
    pub(super) files: std::collections::BTreeMap<(String, String), Stamp>,
}

/// The same finite decoder is used before an observation and before compaction effects.
pub(super) fn inventory(value: &Value, capture: &Capture) -> io::Result<FiniteInventory> {
    let directories = field(value, "directories")?
        .as_array()
        .filter(|a| a.len() <= 2)
        .ok_or_else(|| bad("unbounded archived directories"))?;
    let files = field(value, "files")?
        .as_array()
        .filter(|a| a.len() <= epoch::WINDOW * 2 + 4)
        .ok_or_else(|| bad("unbounded archived files"))?;
    if value.as_object().is_none_or(|m| m.len() != 2) {
        return Err(bad("unknown finite inventory"));
    }
    let mut result = FiniteInventory {
        directories: std::collections::BTreeMap::new(),
        files: std::collections::BTreeMap::new(),
    };
    for directory in directories {
        let name = field(directory, "name")?
            .as_str()
            .ok_or_else(|| bad("archive directory has no name"))?;
        let valid = name
            .strip_prefix("epoch-")
            .and_then(|s| s.strip_suffix(".noindex"))
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > 0 && format!("epoch-{n:016}.noindex") == name);
        let identity = field(directory, "identity")?;
        physical(identity, true)?;
        if !valid
            || directory.as_object().is_none_or(|m| m.len() != 2)
            || result
                .directories
                .insert(name.into(), identity.clone())
                .is_some()
        {
            return Err(bad("duplicate or malformed archive directory"));
        }
    }
    for file in files {
        let directory = field(file, "directory")?
            .as_str()
            .ok_or_else(|| bad("archive file has no parent"))?;
        let name = field(file, "name")?
            .as_str()
            .ok_or_else(|| bad("archive file has an invalid name"))?;
        let stamp = origin(field(file, "stamp")?)?;
        let root = directory.is_empty();
        let valid_name = if root {
            matches!(name, HEADER | MUTATION | CLOSED) || name == capture.name
        } else {
            name == "base.json" || checkpoint_ordinal(name).is_some()
        };
        let mode = if root && (name == MUTATION || name == capture.name) {
            0o600
        } else {
            0o400
        };
        if file.as_object().is_none_or(|m| m.len() != 3)
            || !valid_name
            || stamp.0[3].as_u64().map(|m| m & 0o7777) != Some(mode)
            || (!directory.is_empty() && !result.directories.contains_key(directory))
            || result
                .files
                .insert((directory.into(), name.into()), stamp)
                .is_some()
        {
            return Err(bad("duplicate or malformed archive file"));
        }
    }
    let root_names = [HEADER, MUTATION, CLOSED, capture.name.as_str()];
    if root_names
        .iter()
        .any(|name| !result.files.contains_key(&(String::new(), (*name).into())))
        || result
            .files
            .keys()
            .filter(|(directory, _)| directory.is_empty())
            .count()
            != root_names.len()
        || result.directories.is_empty()
    {
        return Err(bad(
            "archive inventory lacks its mandatory original members",
        ));
    }
    let mut previous = None;
    let mut epochs = result.directories.keys().collect::<Vec<_>>();
    epochs.sort_by_key(|name| epoch_number(name));
    for directory in &epochs {
        let number = epoch_number(directory).expect("directory grammar was checked");
        if previous.is_some_and(|p: u64| p.checked_add(1) != Some(number))
            || result
                .files
                .keys()
                .filter(|(parent, _)| parent == *directory)
                .count()
                > epoch::WINDOW
        {
            return Err(bad(
                "archive epochs are gapped or exceed their finite window",
            ));
        }
        previous = Some(number);
    }
    let current = *epochs.last().unwrap();
    if !result
        .files
        .contains_key(&(current.clone(), "base.json".into()))
    {
        return Err(bad("archive current epoch lacks its original base"));
    }
    Ok(result)
}

pub(super) fn epoch_number(name: &str) -> Option<u64> {
    name.strip_prefix("epoch-")?
        .strip_suffix(".noindex")?
        .parse()
        .ok()
}

pub(super) fn checkpoint_ordinal(name: &str) -> Option<u64> {
    let number = name.strip_suffix(".json")?.parse::<u64>().ok()?;
    (number > 0 && format!("{number:016}.json") == name).then_some(number)
}

pub(super) fn validate(
    namespace: &Namespace,
    requested: &CohortClosureRef,
    slot: &slot::Inspection,
    value: &Value,
    receipt_stamp: &Stamp,
) -> io::Result<Envelope> {
    let terminal = field(value, "terminal")?;
    let reference = CohortClosureRef::from_json(field(value, "reference")?)?;
    let claim = field(value, "claim")?;
    physical(claim, true)?;
    if value.as_object().is_none_or(|m| m.len() != 10)
        || value.get("v").and_then(Value::as_u64) != Some(3)
        || value.get("kind").and_then(Value::as_str) != Some("cohort-archive")
        || reference.namespace() != namespace.nonce()
        || reference.key() != requested.key()
        || value.get("slot") != Some(slot.identity())
        || value.get("slot_gate") != Some(&slot.gate_stamp().value())
        || value.get("active") != Some(&identity(&namespace.open_active()?, true)?)
        || terminal.get("claim") != Some(claim)
        || terminal.as_object().is_none_or(|m| m.len() != 7)
        || terminal.get("v").and_then(Value::as_u64) != Some(3)
        || terminal.get("kind").and_then(Value::as_str) != Some("cohort-terminal")
        || CohortClosureRef::from_json(field(terminal, "reference")?)? != reference
    {
        return Err(bad(
            "archive envelope belongs to a different physical claim",
        ));
    }
    let anchor: Value = aterm_json::from_str(
        field(terminal, "anchor")?
            .as_str()
            .ok_or_else(|| bad("terminal anchor is not text"))?,
    )
    .map_err(|_| bad("terminal claim anchor is malformed"))?;
    physical(field(&anchor, "directory")?, true)?;
    physical(field(&anchor, "lock")?, false)?;
    if anchor.as_object().is_none_or(|m| m.len() != 8)
        || anchor.get("v").and_then(Value::as_u64) != Some(3)
        || anchor.get("namespace").and_then(Value::as_str) != Some(reference.namespace())
        || anchor.get("stem").and_then(Value::as_str) != Some(reference.marker())
        || anchor.get("epoch").and_then(Value::as_str) != Some(reference.epoch())
        || anchor.get("parent") != value.get("active")
        || anchor.get("directory") != Some(claim)
        || anchor.get("guard")
            != Some(&aterm_json::json!([
                reference.guard().device,
                reference.guard().inode
            ]))
    {
        return Err(bad(
            "terminal anchor differs from its stored physical claim",
        ));
    }
    let encoded = aterm_json::to_string(field(terminal, "capture")?)
        .map_err(|_| bad("invalid terminal capture"))?;
    let capture = Capture::decode(&encoded)?;
    origin(&capture.origin.value())?;
    if let Some(previous) = &capture.previous {
        origin(&previous.value())?;
    }
    let (record, journal) = Journal::decode(
        &capture.text,
        capture.name.ends_with(CLAIM),
        namespace.nonce(),
    )?;
    if !capture.retired
        || record.cover_marker() != reference.marker()
        || stem(&capture.name)? != reference.marker()
        || capture.origin.0[0].as_u64() != Some(reference.guard().device)
        || capture.origin.0[1].as_u64() != Some(reference.guard().inode)
        || capture.origin.0[2].as_u64() != Some(capture.text.len() as u64)
    {
        return Err(bad(
            "archive capture differs from its stored original claim",
        ));
    }
    let terminal_origin = origin(field(value, "terminal_origin")?)?;
    let tip = origin(field(terminal, "tip")?)?;
    let text = aterm_json::to_string(terminal).map_err(|_| bad("invalid terminal envelope"))?;
    if terminal_origin.0[2].as_u64() != Some(text.len() as u64)
        || tip.0[2].as_u64() != Some(encoded.len() as u64)
    {
        return Err(bad(
            "terminal origins disagree with their original encoded lengths",
        ));
    }
    // A missing subset is a recovery state only after the exact original inventory was
    // committed. Validate that original intent before any foreign decision or cover.
    let compaction = Compaction::inspect(slot, value, receipt_stamp)?;
    let history = history::History::inspect(slot.history()?, value, compaction.as_ref())?;
    slot.check()?;
    Ok(Envelope {
        reference,
        journal,
        history,
        compaction,
    })
}

pub(super) fn compaction(
    compacted: &Value,
    receipt: &Value,
    receipt_stamp: &Stamp,
    slot: &Value,
) -> io::Result<()> {
    if compacted.as_object().is_none_or(|m| m.len() != 7)
        || compacted.get("v").and_then(Value::as_u64) != Some(3)
        || compacted.get("kind").and_then(Value::as_str) != Some("cohort-terminal-compaction")
        || compacted.get("reference") != receipt.get("reference")
        || compacted.get("slot") != Some(slot)
        || compacted.get("claim") != receipt.get("claim")
        || compacted.get("inventory") != receipt.get("inventory")
        || compacted.get("receipt") != Some(&receipt_stamp.value())
    {
        return Err(bad(
            "compaction receipt differs from original terminal archive",
        ));
    }
    Ok(())
}
