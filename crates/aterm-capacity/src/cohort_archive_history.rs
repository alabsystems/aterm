// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A table of stamps is not deletion authority. Retain and judge every extant member's
//! protocol bytes against the terminal claim before covers or compaction can begin.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct History {
    receipt: Value,
    root: Option<Arc<File>>,
    origin: Option<(u64, u64, u32, i64, i64, i64, i64)>,
    files: Vec<HeldFile>,
    directories: Vec<HeldDirectory>,
}

struct Protocol<'a> {
    receipt: &'a Value,
    reference: CohortClosureRef,
    capture: Capture,
    terminal: String,
    finite: envelope::FiniteInventory,
}

fn text(value: &Value) -> io::Result<String> {
    aterm_json::to_string(value).map_err(|_| bad("invalid archived protocol data"))
}

fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| bad("archived protocol data lacks a required field"))
}

impl<'a> Protocol<'a> {
    fn read(receipt: &'a Value) -> io::Result<Self> {
        let terminal = field(receipt, "terminal")?;
        let reference = CohortClosureRef::from_json(field(receipt, "reference")?)?;
        let anchor: Value = aterm_json::from_str(
            field(terminal, "anchor")?
                .as_str()
                .ok_or_else(|| bad("invalid archived anchor"))?,
        )
        .map_err(|_| bad("invalid archived anchor"))?;
        let capture = Capture::decode(&text(field(terminal, "capture")?)?)?;
        let finite = envelope::inventory(field(receipt, "inventory")?, &capture)?;
        let root = |name: &str| finite.files.get(&(String::new(), name.into()));
        let header = root(HEADER).expect("mandatory inventory member");
        let mutation = root(MUTATION).expect("mandatory inventory member");
        let closed = root(CLOSED).expect("mandatory inventory member");
        let current = finite
            .directories
            .keys()
            .max_by_key(|name| envelope::epoch_number(name))
            .unwrap();
        let tip_name = format!("{:016}.json", capture.ordinal);
        if root(&capture.name) != Some(&capture.origin)
            || closed.value() != *field(receipt, "terminal_origin")?
            || header.0[2].as_u64() != terminal["anchor"].as_str().map(|s| s.len() as u64)
            || text(&anchor)? != terminal["anchor"].as_str().unwrap()
            || mutation.0[2].as_u64() != Some(b"cohort mutation lock v1\n".len() as u64)
            || aterm_json::json!([
                mutation.0[0].clone(),
                mutation.0[1].clone(),
                mutation.0[3].clone(),
                mutation.0[5].clone()
            ]) != *field(&anchor, "lock")?
            || finite
                .files
                .get(&(current.clone(), tip_name))
                .map(Stamp::value)
                != Some(field(terminal, "tip")?.clone())
            || finite.files.keys().any(|(parent, name)| {
                !parent.is_empty()
                    && envelope::checkpoint_ordinal(name).is_some_and(|n| n > capture.ordinal)
            })
        {
            return Err(bad(
                "archive inventory differs from its terminal claim origins",
            ));
        }
        let protocol = Self {
            receipt,
            reference,
            capture,
            terminal: text(terminal)?,
            finite,
        };
        protocol.capture(&protocol.capture)?;
        Ok(protocol)
    }

    fn capture(&self, capture: &Capture) -> io::Result<()> {
        envelope::origin(&capture.origin.value())?;
        if let Some(previous) = &capture.previous {
            envelope::origin(&previous.value())?;
            if previous.0[3].as_u64().map(|mode| mode & 0o7777) != Some(0o400) {
                return Err(bad("archived checkpoint has a non-metadata predecessor"));
            }
        }
        let (record, _) = Journal::decode(
            &capture.text,
            capture.name.ends_with(CLAIM),
            self.reference.namespace(),
        )?;
        if capture.ordinal == 0
            || capture.ordinal > self.capture.ordinal
            || stem(&capture.name)? != self.reference.marker()
            || record.cover_marker() != self.reference.marker()
            || capture.origin.0[..2] != self.capture.origin.0[..2]
            || capture.origin.0[3..6] != self.capture.origin.0[3..6]
            || capture.origin.0[2].as_u64() != Some(capture.text.len() as u64)
            // The immutable declaration never changes. Semantic updates are checked
            // between adjacent original generations below: complete snapshots need not
            // retain a textual prefix of all earlier custody events.
            || capture.text.lines().next() != self.capture.text.lines().next()
            || (capture.retired && capture != &self.capture)
        {
            return Err(bad(
                "archived generation does not belong to the original terminal claim",
            ));
        }
        Ok(())
    }

    fn root(&self, name: &str, body: &str, stamp: &Stamp) -> io::Result<()> {
        let valid = match name {
            HEADER => body == self.receipt["terminal"]["anchor"].as_str().unwrap(),
            MUTATION => body == "cohort mutation lock v1\n",
            CLOSED => body == self.terminal,
            _ => {
                name == self.capture.name
                    && body == self.capture.text
                    && stamp == &self.capture.origin
            }
        };
        if !valid {
            return Err(bad(
                "unknown payload under an archived protocol name is preserved",
            ));
        }
        Ok(())
    }

    fn base(&self, directory: &str, body: &str) -> io::Result<Capture> {
        let value: Value =
            aterm_json::from_str(body).map_err(|_| bad("invalid archived epoch base"))?;
        let number = envelope::epoch_number(directory).unwrap();
        let base = Capture::decode(&text(field(&value, "capture")?)?)?;
        self.capture(&base)?;
        if value.as_object().is_none_or(|v| v.len() != 7)
            || value.get("v").and_then(Value::as_u64) != Some(3)
            || value.get("kind").and_then(Value::as_str) != Some("cohort-epoch-base")
            || value.get("epoch").and_then(Value::as_u64) != Some(number)
            || value.get("claim") != self.receipt.get("claim")
            || value.get("directory") != self.finite.directories.get(directory)
            || base.retired
            || text(&value)? != body
        {
            return Err(bad(
                "archived epoch base is not canonical original claim data",
            ));
        }
        match field(&value, "predecessor")? {
            Value::Null if number == 1 && base.ordinal == 1 && base.previous.is_none() => {}
            predecessor if number > 1 => {
                let old = number - 1;
                let old_name = format!("epoch-{old:016}.noindex");
                let members = field(predecessor, "members")?
                    .as_array()
                    .filter(|v| !v.is_empty() && v.len() <= epoch::WINDOW)
                    .ok_or_else(|| bad("unbounded archived predecessor"))?;
                envelope::physical(field(predecessor, "directory")?, true)?;
                envelope::origin(field(predecessor, "tip")?)?;
                if predecessor.as_object().is_none_or(|v| v.len() != 4)
                    || predecessor.get("epoch").and_then(Value::as_u64) != Some(old)
                    || !predecessor
                        .get("directory")
                        .is_some_and(|v| v.as_array().is_some_and(|a| a.len() == 4))
                {
                    return Err(bad("invalid archived predecessor"));
                }
                let mut declared = BTreeMap::new();
                let mut last = None;
                for member in members {
                    let name = field(member, "name")?
                        .as_str()
                        .filter(|n| *n == "base.json" || envelope::checkpoint_ordinal(n).is_some())
                        .ok_or_else(|| bad("unknown archived predecessor member"))?;
                    let stamp = envelope::origin(field(member, "stamp")?)?;
                    if member.as_object().is_none_or(|v| v.len() != 2)
                        || stamp.0[3].as_u64().map(|mode| mode & 0o7777) != Some(0o400)
                        || last.is_some_and(|p: &str| p >= name)
                        || declared.insert(name, stamp).is_some()
                    {
                        return Err(bad("noncanonical archived predecessor inventory"));
                    }
                    last = Some(name);
                }
                if !declared.values().any(|s| s.value() == predecessor["tip"])
                    || self
                        .finite
                        .directories
                        .get(&old_name)
                        .is_some_and(|id| Some(id) != predecessor.get("directory"))
                    || self
                        .finite
                        .files
                        .iter()
                        .filter(|((parent, _), _)| parent == &old_name)
                        .any(|((_, name), stamp)| declared.get(name.as_str()) != Some(stamp))
                {
                    return Err(bad(
                        "archived predecessor differs from original finite members",
                    ));
                }
            }
            _ => return Err(bad("archived epoch lacks its original predecessor")),
        }
        Ok(base)
    }
}

impl History {
    pub(super) fn inspect(
        root: Option<File>,
        receipt: &Value,
        compaction: Option<&envelope::Compaction>,
    ) -> io::Result<Self> {
        Self::inspect_original(root, receipt, compaction, None)
    }

    pub(super) fn inspect_hot(
        root: Option<File>,
        receipt: &Value,
        original: &Inventory,
    ) -> io::Result<Self> {
        let history = Self::inspect_original(root, receipt, None, Some(original))?;
        original.recheck()?;
        Ok(history)
    }

    fn inspect_original(
        root: Option<File>,
        receipt: &Value,
        compaction: Option<&envelope::Compaction>,
        original: Option<&Inventory>,
    ) -> io::Result<Self> {
        let protocol = Protocol::read(receipt)?;
        if let Some(compaction) = compaction {
            compaction.require_receipt(receipt)?;
        }
        let root = match (root, original) {
            (Some(named), Some(original)) => Some(original.share_root(&named)?),
            (None, Some(_)) => return Err(bad("hot archive lost its original history")),
            (root, None) => root.map(Arc::new),
        };
        let mut history = Self {
            receipt: receipt.clone(),
            root,
            origin: None,
            files: Vec::new(),
            directories: Vec::new(),
        };
        let Some(root) = &history.root else {
            return Ok(history);
        };
        if identity(root, true)? != *field(receipt, "claim")? {
            return Err(bad(
                "archived history differs from its stored original claim",
            ));
        }
        history.origin = Some(directory_origin(root)?);
        let mut seen = BTreeSet::new();
        let mut generations = BTreeMap::new();
        for item in fd::Dir::read_from(root.as_fd())? {
            let item = item?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 archived history member"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if !seen.insert(name.to_owned()) || seen.len() > 6 {
                return Err(bad("unbounded archived history"));
            }
            if let Some(expected) = protocol.finite.directories.get(name) {
                let directory = {
                    let named = open(root, name, false, true)?;
                    match original {
                        Some(original) => original.share_directory(root, name, &named)?,
                        None => Arc::new(named),
                    }
                };
                let origin = directory_origin(&directory)?;
                if identity(&directory, true)? != *expected {
                    return Err(bad("archived epoch directory was replaced"));
                }
                let mut captures = BTreeMap::new();
                let mut base = None;
                for child in fd::Dir::read_from(directory.as_fd())? {
                    let child = child?;
                    let child = child
                        .file_name()
                        .to_str()
                        .map_err(|_| bad("non-UTF8 archived epoch member"))?;
                    if matches!(child, "." | "..") {
                        continue;
                    }
                    let expected = protocol
                        .finite
                        .files
                        .get(&(name.to_owned(), child.to_owned()))
                        .ok_or_else(|| bad("unknown epoch payload is preserved"))?;
                    let (file, body, stamp) = match original {
                        Some(original) => original.share_member(&directory, child, true)?,
                        None => {
                            let (file, body, stamp) = held_immutable(&directory, child)?;
                            (Arc::new(file), body, stamp)
                        }
                    };
                    if stamp != *expected {
                        return Err(bad("archived epoch member changed or was restored"));
                    }
                    if child == "base.json" {
                        let capture = protocol.base(name, &body)?;
                        remember_generation(&mut generations, &capture)?;
                        base = Some((capture, stamp.clone()));
                    } else {
                        let ordinal = envelope::checkpoint_ordinal(child)
                            .ok_or_else(|| bad("unknown archived checkpoint name"))?;
                        let capture = Capture::decode(&body)?;
                        protocol.capture(&capture)?;
                        remember_generation(&mut generations, &capture)?;
                        if capture.ordinal != ordinal {
                            return Err(bad("archived checkpoint ordinal differs from its name"));
                        }
                        if capture.ordinal == protocol.capture.ordinal
                            && (capture != protocol.capture
                                || stamp.value() != receipt["terminal"]["tip"])
                        {
                            return Err(bad(
                                "archived tip differs from its retained terminal capture",
                            ));
                        }
                        captures.insert(ordinal, (capture, stamp.clone()));
                    }
                    history.files.push(HeldFile {
                        parent: Arc::clone(&directory),
                        name: child.into(),
                        file,
                        stamp,
                    });
                }
                if let Some((base, stamp)) = base {
                    let mut ordinals = protocol
                        .finite
                        .files
                        .keys()
                        .filter(|(parent, child)| parent == name && child != "base.json")
                        .map(|(_, child)| {
                            envelope::checkpoint_ordinal(child).expect("finite grammar checked")
                        })
                        .collect::<Vec<_>>();
                    ordinals.sort_unstable();
                    let first = base
                        .ordinal
                        .checked_add(1)
                        .ok_or_else(|| bad("archived ordinal range exhausted"))?;
                    let current = protocol
                        .finite
                        .directories
                        .keys()
                        .max_by_key(|directory| envelope::epoch_number(directory))
                        .unwrap();
                    // The current table is complete at transfer. A predecessor's table
                    // may already be a subset after an interrupted earlier epoch prune.
                    if name == current
                        && ordinals
                            .iter()
                            .copied()
                            .zip(first..)
                            .any(|(actual, expected)| actual != expected)
                    {
                        return Err(bad(
                            "archived epoch inventory has a gapped or exhausted ordinal range",
                        ));
                    }
                    let mut previous = (base.ordinal, stamp, base.text);
                    for (ordinal, (capture, stamp)) in captures {
                        let original_previous = protocol
                            .finite
                            .files
                            .get(&(name.to_owned(), format!("{:016}.json", ordinal - 1)))
                            .or_else(|| (ordinal == first).then_some(&previous.1));
                        if ordinal <= previous.0
                            || (ordinal == previous.0 + 1
                                && (capture.previous.as_ref() != Some(&previous.1)
                                    || !known_record_update(&previous.2, &capture.text)))
                            || original_previous
                                .is_some_and(|expected| capture.previous.as_ref() != Some(expected))
                        {
                            return Err(bad(
                                "archived epoch has a changed or reversed checkpoint chain",
                            ));
                        }
                        previous = (ordinal, stamp, capture.text);
                    }
                }
                history.directories.push(HeldDirectory {
                    parent: Arc::clone(root),
                    name: name.into(),
                    file: directory,
                    identity: expected.clone(),
                    origin,
                });
            } else {
                let expected = protocol
                    .finite
                    .files
                    .get(&(String::new(), name.into()))
                    .ok_or_else(|| bad("unknown claim payload is preserved"))?;
                let (file, body, stamp) = match original {
                    Some(original) => original.share_member(root, name, false)?,
                    None => {
                        let file = open(root, name, false, false)?;
                        let (body, stamp) = bytes(&file, MAX_ENTRY)?;
                        (Arc::new(file), body, stamp)
                    }
                };
                if stamp != *expected {
                    return Err(bad("archived claim member changed or was restored"));
                }
                protocol.root(name, &body, &stamp)?;
                history.files.push(HeldFile {
                    parent: Arc::clone(root),
                    name: name.into(),
                    file,
                    stamp,
                });
            }
        }
        if compaction.is_none()
            && (history.files.len() != protocol.finite.files.len()
                || history.directories.len() != protocol.finite.directories.len())
        {
            return Err(bad(
                "archive lost original members before any committed compaction intent",
            ));
        }
        history.recheck()?;
        Ok(history)
    }

    pub(super) fn recheck(&self) -> io::Result<()> {
        if let Some(root) = &self.root
            && Some(directory_origin(root)?) != self.origin
        {
            return Err(bad("archived history changed after its protocol read"));
        }
        for directory in &self.directories {
            if identity(
                &open(&directory.parent, &directory.name, false, true)?,
                true,
            )? != directory.identity
                || directory_origin(&directory.file)? != directory.origin
            {
                return Err(bad("archived epoch changed after its protocol read"));
            }
        }
        for file in &self.files {
            if Stamp::read(&file.file)? != file.stamp
                || Stamp::read(&open(&file.parent, &file.name, false, false)?)? != file.stamp
            {
                return Err(bad("archived member changed after its protocol read"));
            }
        }
        Ok(())
    }

    pub(super) fn compact(
        &mut self,
        slot: &mut Slot,
        intent: &envelope::CoveredCompaction,
    ) -> io::Result<()> {
        intent.require(slot, &self.receipt)?;
        self.recheck()?;
        let Some(root) = &self.root else {
            return Ok(());
        };
        for file in std::mem::take(&mut self.files) {
            if Stamp::read(&file.file)? != file.stamp
                || Stamp::read(&open(&file.parent, &file.name, false, false)?)? != file.stamp
            {
                return Err(bad("compaction member changed before its known unlink"));
            }
            fd::unlinkat(
                file.parent.as_ref(),
                file.name.as_str(),
                fd::AtFlags::empty(),
            )?;
            #[cfg(test)]
            boundary("compaction-unlinked")?;
        }
        for directory in std::mem::take(&mut self.directories) {
            aterm_durable::durable(&directory.file)?;
            if identity(&open(root, &directory.name, false, true)?, true)? != directory.identity {
                return Err(bad("compaction epoch changed before known removal"));
            }
            fd::unlinkat(
                root.as_ref(),
                directory.name.as_str(),
                fd::AtFlags::REMOVEDIR,
            )?;
        }
        aterm_durable::durable(root)?;
        let claim = identity(root, true)?;
        if identity(&open(slot.file(), HISTORY, false, true)?, true)? != claim {
            return Err(bad("compaction history changed before known removal"));
        }
        slot.remove_history(&claim)?;
        self.root = None;
        self.origin = None;
        Ok(())
    }
}

/// Rotation copies the SAME complete capture into the successor epoch base. Original
/// tip/base files may have different metadata stamps, but never different semantic bytes.
fn remember_generation(
    generations: &mut BTreeMap<u64, Capture>,
    capture: &Capture,
) -> io::Result<()> {
    if generations
        .get(&capture.ordinal)
        .is_some_and(|known| known != capture)
    {
        return Err(bad(
            "archived epoch base differs from its original generation",
        ));
    }
    generations.insert(capture.ordinal, capture.clone());
    Ok(())
}
