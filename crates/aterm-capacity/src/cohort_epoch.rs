// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded physical epochs beneath a claim anchor. A new base copies the SAME validated
//! record/origin; it never changes journal state or captures a newer working generation.

use super::*;

pub(super) const WINDOW: usize = 32;
const BASE: &str = "base.json";

#[derive(Clone, Debug, PartialEq)]
struct Member {
    name: String,
    stamp: Stamp,
}

#[derive(Clone, Debug)]
struct Predecessor {
    epoch: u64,
    directory: Value,
    tip: Stamp,
    members: Vec<Member>,
}

#[derive(Debug)]
pub(super) struct Epoch {
    pub(super) number: u64,
    pub(super) directory: File,
    pub(super) base: Capture,
    base_file: File,
    base_stamp: Stamp,
    predecessor: Option<Predecessor>,
}

/// The inventory is captured from original descriptors before publication and is kept
/// alive through both covering flushes. A later pathname sweep cannot replace this proof.
struct Inventory<'a> {
    epoch: &'a Epoch,
    origin: (u64, u64, u32, i64, i64, i64, i64),
    files: Vec<(String, File, Stamp)>,
}

impl Inventory<'_> {
    fn recheck(&self, root: &File) -> io::Result<()> {
        self.epoch.recheck_origin(root)?;
        if directory_origin(&self.epoch.directory)? != self.origin {
            return Err(bad("predecessor directory changed across base publication"));
        }
        for (name, held, stamp) in &self.files {
            if Stamp::read(held)? != *stamp
                || Stamp::read(&open(&self.epoch.directory, name, false, false)?)? != *stamp
            {
                return Err(bad("predecessor member changed across base publication"));
            }
        }
        Ok(())
    }
}

fn name(number: u64) -> String {
    format!("epoch-{number:016}.noindex")
}
fn preparing(number: u64) -> String {
    format!("epoch-{number:016}.preparing.noindex")
}

fn capture_value(capture: &Capture) -> io::Result<Value> {
    aterm_json::from_str(&capture.encode()?).map_err(|_| bad("invalid epoch capture"))
}

impl Predecessor {
    fn encode(&self) -> Value {
        aterm_json::json!({"epoch":self.epoch,"directory":self.directory,"tip":self.tip.value(),
            "members":self.members.iter().map(|m| aterm_json::json!({"name":m.name,"stamp":m.stamp.value()})).collect::<Vec<_>>()})
    }

    fn decode(value: &Value) -> io::Result<Self> {
        if value.as_object().is_none_or(|m| m.len() != 4) {
            return Err(bad("invalid predecessor inventory"));
        }
        let epoch = value
            .get("epoch")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| bad("missing predecessor epoch"))?;
        let directory = value
            .get("directory")
            .filter(|v| v.as_array().is_some_and(|a| a.len() == 4))
            .ok_or_else(|| bad("missing predecessor identity"))?
            .clone();
        let tip = Stamp::decode(
            value
                .get("tip")
                .ok_or_else(|| bad("missing predecessor tip"))?,
        )?;
        let values = value
            .get("members")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty() && a.len() <= WINDOW)
            .ok_or_else(|| bad("unbounded predecessor inventory"))?;
        let mut members = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for value in values {
            let member_name = value
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("missing predecessor name"))?;
            if value.as_object().is_none_or(|m| m.len() != 2)
                || !entry_name(member_name)
                || !names.insert(member_name.to_owned())
            {
                return Err(bad("unknown or duplicate predecessor member"));
            }
            members.push(Member {
                name: member_name.into(),
                stamp: Stamp::decode(
                    value
                        .get("stamp")
                        .ok_or_else(|| bad("missing predecessor stamp"))?,
                )?,
            });
        }
        if !members.windows(2).all(|pair| pair[0].name < pair[1].name)
            || !members.iter().any(|member| member.stamp == tip)
        {
            return Err(bad(
                "predecessor inventory has no exact tip or canonical order",
            ));
        }
        Ok(Self {
            epoch,
            directory,
            tip,
            members,
        })
    }
}

fn entry_name(name: &str) -> bool {
    if name == BASE {
        return true;
    }
    let Some(number) = name.strip_suffix(".json") else {
        return false;
    };
    number
        .parse::<u64>()
        .ok()
        .is_some_and(|n| n > 0 && format!("{n:016}") == number)
}

impl Epoch {
    fn base_text(
        root: &File,
        directory: &File,
        number: u64,
        capture: &Capture,
        predecessor: Option<&Predecessor>,
    ) -> io::Result<String> {
        aterm_json::to_string(&aterm_json::json!({"v":3u64,"kind":"cohort-epoch-base", "epoch":number,
            "claim":identity(root,true)?,"directory":identity(directory,true)?,"capture":capture_value(capture)?,
            "predecessor":predecessor.map(Predecessor::encode)})).map_err(|_| bad("cannot encode epoch base"))
    }

    pub(super) fn first(
        root: &File,
        backend: &File,
        writer: &File,
        capture: &Capture,
    ) -> io::Result<Self> {
        if capture.ordinal != 1 || capture.previous.is_some() || capture.retired {
            return Err(bad("initial epoch has no initial capture"));
        }
        Self::publish(root, backend, writer, 1, capture.clone(), None, None)
    }

    fn publish(
        root: &File,
        backend: &File,
        writer: &File,
        number: u64,
        capture: Capture,
        predecessor: Option<Predecessor>,
        old: Option<&Inventory<'_>>,
    ) -> io::Result<Self> {
        if let Some(old) = old {
            let prepared = begin_rotation(
                root,
                backend,
                writer,
                number,
                capture,
                predecessor.ok_or_else(|| bad("rotation lacks its original predecessor"))?,
                old,
            )?;
            let covered = cover_rotation(prepared, root, backend, writer, old)?;
            publish_rotation(covered, root, backend, writer, old)
        } else {
            let prepared =
                PreparedEpoch::new(root, backend, writer, number, capture, predecessor, None)?;
            prepared
                .cover(root, backend, writer, None)?
                .publish(root, backend, writer, None)
        }
    }

    /// The caller supplies the current tip's original descriptor/stamp and capture while
    /// holding the claim mutation lock. No working-path recapture is performed here.
    pub(super) fn rotate(
        &self,
        root: &File,
        backend: &File,
        writer: &File,
        tip: &Capture,
        tip_file: &File,
        tip_stamp: &Stamp,
    ) -> io::Result<Self> {
        self.prune_predecessor(root)?;
        self.recheck(root)?;
        tip.check(backend, writer)?;
        if Stamp::read(tip_file)? != *tip_stamp {
            return Err(bad("epoch tip lost its original origin"));
        }
        let before = directory_origin(&self.directory)?;
        let mut members = Vec::new();
        let mut files = Vec::new();
        for entry in fd::Dir::read_from(self.directory.as_fd())? {
            let entry = entry?;
            let entry = entry
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 epoch member"))?;
            if matches!(entry, "." | "..") {
                continue;
            }
            if !entry_name(entry) || members.len() >= WINDOW {
                return Err(bad("epoch inventory is unknown or exceeds its window"));
            }
            let (file, _, stamp) = held_immutable(&self.directory, entry)?;
            if stamp.mode() & 0o111 != 0 {
                return Err(bad("epoch inventory contains an executable"));
            }
            members.push(Member {
                name: entry.into(),
                stamp: stamp.clone(),
            });
            files.push((entry.into(), file, stamp));
        }
        if directory_origin(&self.directory)? != before {
            return Err(bad("epoch changed during predecessor capture"));
        }
        if !members.iter().any(|m| m.stamp == *tip_stamp) {
            return Err(bad("current tip is not in its predecessor epoch"));
        }
        members.sort_by(|a, b| a.name.cmp(&b.name));
        let predecessor = Predecessor {
            epoch: self.number,
            directory: identity(&self.directory, true)?,
            tip: tip_stamp.clone(),
            members,
        };
        let inventory = Inventory {
            epoch: self,
            origin: before,
            files,
        };
        inventory.recheck(root)?;
        let number = self
            .number
            .checked_add(1)
            .ok_or_else(|| bad("epoch number exhausted"))?;
        Self::publish(
            root,
            backend,
            writer,
            number,
            tip.clone(),
            Some(predecessor),
            Some(&inventory),
        )
    }

    pub(super) fn open(
        root: &File,
        expected_stem: &str,
        recover: bool,
    ) -> io::Result<Option<Self>> {
        if recover {
            Self::recover_preparation(root, expected_stem)?;
        }
        let numbers = epoch_numbers(root, expected_stem)?;
        let Some(&number) = numbers.last() else {
            return Ok(None);
        };
        let epoch = Self::read_base(root, &name(number), number)?;
        epoch.recheck(root)?;
        epoch.check_predecessor(root)?;
        Ok(Some(epoch))
    }

    fn read_base(root: &File, named: &str, number: u64) -> io::Result<Self> {
        let parsed = Self::read_base_uncovered(root, named, number)?;
        parsed.recheck(root, named)?;
        cover_frozen(
            &parsed.epoch.directory,
            BASE,
            &parsed.epoch.base_file,
            &parsed.text,
            &parsed.epoch.base_stamp,
        )?;
        parsed.recheck(root, named)?;
        Ok(parsed.epoch)
    }

    fn read_base_uncovered(root: &File, named: &str, number: u64) -> io::Result<ParsedBase> {
        let directory = open(root, named, false, true)?;
        let origin = directory_origin(&directory)?;
        let (base_file, text, base_stamp) = held_immutable(&directory, BASE)?;
        let value: Value = aterm_json::from_str(&text).map_err(|_| bad("torn epoch base"))?;
        if value.as_object().is_none_or(|m| m.len() != 7)
            || value.get("v").and_then(Value::as_u64) != Some(3)
            || value.get("kind").and_then(Value::as_str) != Some("cohort-epoch-base")
            || value.get("epoch").and_then(Value::as_u64) != Some(number)
            || value.get("claim") != Some(&identity(root, true)?)
            || value.get("directory") != Some(&identity(&directory, true)?)
        {
            return Err(bad("epoch base was copied or has an unknown shape"));
        }
        let encoded = aterm_json::to_string(
            value
                .get("capture")
                .ok_or_else(|| bad("epoch base has no capture"))?,
        )
        .map_err(|_| bad("cannot read epoch capture"))?;
        let base = Capture::decode(&encoded)?;
        let predecessor = match value.get("predecessor") {
            Some(Value::Null) => None,
            Some(value) => Some(Predecessor::decode(value)?),
            None => return Err(bad("epoch base has no predecessor declaration")),
        };
        if (number == 1 && (predecessor.is_some() || base.ordinal != 1 || base.previous.is_some()))
            || (number > 1
                && predecessor
                    .as_ref()
                    .is_none_or(|p| p.epoch.checked_add(1) != Some(number)))
            || base.retired
            || Self::base_text(root, &directory, number, &base, predecessor.as_ref())? != text
        {
            return Err(bad("epoch base has a gapped or noncanonical predecessor"));
        }
        let parsed = ParsedBase {
            epoch: Self {
                number,
                directory,
                base,
                base_file,
                base_stamp,
                predecessor,
            },
            text,
            origin,
        };
        parsed.recheck(root, named)?;
        Ok(parsed)
    }

    /// A complete preparation already declares its exact predecessor descriptors and
    /// unchanged working origin. Complete that SAME transition under the claim lock; a
    /// torn base, missing predecessor, altered file or ordinal gap remains unknown.
    fn recover_preparation(root: &File, expected_stem: &str) -> io::Result<()> {
        let before = directory_origin(root)?;
        let mut pending = None;
        let mut final_numbers = Vec::new();
        let mut working = 0;
        for item in fd::Dir::read_from(root.as_fd())? {
            let item = item?;
            let child = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 claim child"))?;
            if matches!(child, "." | ".." | HEADER | MUTATION | "closed.json") {
                continue;
            }
            if stem(child).is_ok_and(|s| s == expected_stem) {
                working += 1;
                if working > 1 {
                    return Err(bad("claim root contains multiple working records"));
                }
                continue;
            }
            if let Some(raw) = child
                .strip_prefix("epoch-")
                .and_then(|s| s.strip_suffix(".preparing.noindex"))
            {
                let number = raw
                    .parse::<u64>()
                    .map_err(|_| bad("malformed prepared epoch"))?;
                if number == 0 || preparing(number) != child || pending.replace(number).is_some() {
                    return Err(bad("duplicate or malformed prepared epoch"));
                }
            } else {
                let number = child
                    .strip_prefix("epoch-")
                    .and_then(|s| s.strip_suffix(".noindex"))
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|n| *n > 0 && name(*n) == child)
                    .ok_or_else(|| bad("unknown claim entry"))?;
                if final_numbers.len() >= 2 {
                    return Err(bad("unbounded published epochs"));
                }
                final_numbers.push(number);
            }
        }
        let Some(number) = pending else { return Ok(()) };
        if directory_origin(root)? != before || final_numbers.len() > 1 {
            return Err(bad("epoch preparation has an unresolved older prefix"));
        }
        let parsed = Self::read_base_uncovered(root, &preparing(number), number)?;
        let prepared_origin = parsed.origin;
        let prepared_text = parsed.text;
        let prepared = parsed.epoch;
        if preparation_origin(&prepared.directory)? != prepared_origin {
            return Err(bad(
                "prepared directory changed across its original semantic read",
            ));
        }
        let mut original = None;
        let mut files = Vec::new();
        if let Some(predecessor) = &prepared.predecessor {
            if final_numbers.as_slice() != [predecessor.epoch] {
                return Err(bad("prepared epoch lost its exact predecessor"));
            }
            let old = Self::read_base(root, &name(predecessor.epoch), predecessor.epoch)?;
            if identity(&old.directory, true)? != predecessor.directory {
                return Err(bad("prepared predecessor directory changed"));
            }
            let origin = directory_origin(&old.directory)?;
            let mut count = 0;
            for item in fd::Dir::read_from(old.directory.as_fd())? {
                let item = item?;
                let child = item
                    .file_name()
                    .to_str()
                    .map_err(|_| bad("non-UTF8 prepared predecessor"))?;
                if matches!(child, "." | "..") {
                    continue;
                }
                count += 1;
                let expected = predecessor
                    .members
                    .iter()
                    .find(|m| m.name == child)
                    .ok_or_else(|| bad("prepared predecessor gained an unknown entry"))?;
                let (file, text, stamp) = held_immutable(&old.directory, child)?;
                if stamp != expected.stamp {
                    return Err(bad("prepared predecessor changed or was restored"));
                }
                if stamp == predecessor.tip {
                    let capture = if child == BASE {
                        let value: Value = aterm_json::from_str(&text)
                            .map_err(|_| bad("torn predecessor base"))?;
                        Capture::decode(
                            &aterm_json::to_string(
                                value
                                    .get("capture")
                                    .ok_or_else(|| bad("predecessor has no base capture"))?,
                            )
                            .map_err(|_| bad("invalid predecessor capture"))?,
                        )?
                    } else {
                        Capture::decode(&text)?
                    };
                    if capture != prepared.base {
                        return Err(bad(
                            "prepared base does not equal the original predecessor tip",
                        ));
                    }
                }
                files.push((child.to_owned(), file, stamp));
            }
            if count != predecessor.members.len() || directory_origin(&old.directory)? != origin {
                return Err(bad(
                    "prepared predecessor inventory is incomplete or changed",
                ));
            }
            original = Some((old, origin));
        } else if !final_numbers.is_empty() || number != 1 {
            return Err(bad("initial preparation has an unrelated epoch"));
        }
        let inventory = original.as_ref().map(|(epoch, origin)| Inventory {
            epoch,
            origin: *origin,
            files,
        });
        let record = open(root, &prepared.base.name, false, false)?;
        prepared.base.check(root, &record)?;
        if let Some(inventory) = &inventory {
            inventory.recheck(root)?
        }
        aterm_durable::durable(&record)?;
        aterm_durable::durable(root)?;
        prepared.base.check(root, &record)?;
        let prepared = PreparedEpoch {
            epoch: prepared,
            text: prepared_text,
            origin: prepared_origin,
        };
        if let Some(inventory) = &inventory {
            let covered = cover_rotation(prepared, root, root, &record, inventory)?;
            publish_rotation(covered, root, root, &record, inventory)?;
        } else {
            prepared
                .cover(root, root, &record, None)?
                .publish(root, root, &record, None)?;
        }
        Ok(())
    }

    pub(super) fn base_descriptor(&self) -> io::Result<(File, Stamp)> {
        Ok((self.base_file.try_clone()?, self.base_stamp.clone()))
    }

    pub(super) fn cover_base(&self, root: &File) -> io::Result<()> {
        let text = Self::base_text(
            root,
            &self.directory,
            self.number,
            &self.base,
            self.predecessor.as_ref(),
        )?;
        cover_frozen(
            &self.directory,
            BASE,
            &self.base_file,
            &text,
            &self.base_stamp,
        )?;
        self.recheck(root)
    }

    fn check_predecessor(&self, root: &File) -> io::Result<()> {
        let Some(predecessor) = &self.predecessor else {
            return Ok(());
        };
        let old = match open(root, &name(predecessor.epoch), false, true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if identity(&old, true)? != predecessor.directory {
            return Err(bad("predecessor epoch was replaced"));
        }
        let before = directory_origin(&old)?;
        let mut checked = Vec::new();
        for item in fd::Dir::read_from(old.as_fd())? {
            let item = item?;
            let child = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 predecessor member"))?;
            if matches!(child, "." | "..") {
                continue;
            }
            let expected = predecessor
                .members
                .iter()
                .find(|m| m.name == child)
                .ok_or_else(|| bad("unknown predecessor member"))?;
            let (held, _, stamp) = held_immutable(&old, child)?;
            if stamp != expected.stamp {
                return Err(bad("predecessor member changed or was restored"));
            }
            checked.push((child.to_owned(), held, stamp));
        }
        for (child, held, stamp) in checked {
            if Stamp::read(&held)? != stamp
                || Stamp::read(&open(&old, &child, false, false)?)? != stamp
            {
                return Err(bad("predecessor changed across read"));
            }
        }
        if directory_origin(&old)? != before {
            return Err(bad("predecessor directory changed across read"));
        }
        Ok(())
    }

    pub(super) fn recheck(&self, root: &File) -> io::Result<()> {
        if epoch_numbers(root, stem(&self.base.name)?)?.last() != Some(&self.number) {
            return Err(bad(
                "held epoch is superseded or its namespace is incomplete",
            ));
        }
        self.recheck_origin(root)
    }

    fn recheck_origin(&self, root: &File) -> io::Result<()> {
        if identity(&open(root, &name(self.number), false, true)?, true)?
            != identity(&self.directory, true)?
            || Stamp::read(&self.base_file)? != self.base_stamp
            || Stamp::read(&open(&self.directory, BASE, false, false)?)? != self.base_stamp
        {
            return Err(bad(
                "held epoch anchor is no longer current at its original name",
            ));
        }
        Ok(())
    }

    /// A superseded directory may be absent or a subset of the finite covered inventory.
    /// Unknown data is preserved. A later rotation cannot skip unfinished old cleanup.
    pub(super) fn prune_predecessor(&self, root: &File) -> io::Result<()> {
        let Some(predecessor) = &self.predecessor else {
            return Ok(());
        };
        self.check_predecessor(root)?;
        let old_name = name(predecessor.epoch);
        let old = match open(root, &old_name, false, true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if identity(&old, true)? != predecessor.directory {
            return Err(bad("superseded epoch changed identity"));
        }
        let mut remaining = Vec::new();
        for item in fd::Dir::read_from(old.as_fd())? {
            let item = item?;
            let child = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 superseded member"))?;
            if matches!(child, "." | "..") {
                continue;
            }
            let expected = predecessor
                .members
                .iter()
                .find(|member| member.name == child)
                .ok_or_else(|| bad("unknown superseded epoch member"))?;
            let (held, _, stamp) = held_immutable(&old, child)?;
            if stamp != expected.stamp {
                return Err(bad("superseded member changed or was restored"));
            }
            remaining.push((child.to_owned(), held, stamp));
        }
        for (child, held, stamp) in remaining {
            prune_member(&old, &child, &held, &stamp)?;
        }
        aterm_durable::durable(&old)?;
        self.recheck(root)?;
        remove_empty_predecessor(root, &old_name, &predecessor.directory)
    }
}

/// An uncovered semantic read carries the directory origin from BEFORE opening the
/// base. Published readers cover it directly; recovery transfers these exact facts into
/// a preparation before performing any base cover.
struct ParsedBase {
    epoch: Epoch,
    text: String,
    origin: (u64, u64, u32, i64, i64, i64, i64),
}

impl ParsedBase {
    fn recheck(&self, root: &File, named: &str) -> io::Result<()> {
        if identity(&open(root, named, false, true)?, true)?
            != identity(&self.epoch.directory, true)?
            || directory_origin(&self.epoch.directory)? != self.origin
            || Stamp::read(&self.epoch.base_file)? != self.epoch.base_stamp
            || Stamp::read(&open(&self.epoch.directory, BASE, false, false)?)?
                != self.epoch.base_stamp
        {
            return Err(bad(
                "epoch base or directory changed after its original semantic read",
            ));
        }
        Ok(())
    }
}

/// A complete unpublished base retains the descriptors and stamps from its own write or
/// validated recovery read. It cannot become a published epoch without the covering stage.
struct PreparedEpoch {
    epoch: Epoch,
    text: String,
    origin: (u64, u64, u32, i64, i64, i64, i64),
}

struct CoveredEpoch(PreparedEpoch);

#[cfg(test)]
type OriginalBaseCover = (Value, fn(&File) -> io::Result<()>);
#[cfg(test)]
thread_local! {
    static ORIGINAL_BASE_COVER: std::cell::RefCell<Option<OriginalBaseCover>> = const {
        std::cell::RefCell::new(None)
    };
}
#[cfg(test)]
pub(super) fn observe_original_base_cover(parent: &File, observe: fn(&File) -> io::Result<()>) {
    ORIGINAL_BASE_COVER
        .with(|hook| *hook.borrow_mut() = Some((identity(parent, true).unwrap(), observe)));
}
#[cfg(test)]
pub(super) fn after_original_base_cover(parent: &File) -> io::Result<()> {
    let matches = ORIGINAL_BASE_COVER.with(|hook| {
        hook.borrow()
            .as_ref()
            .is_some_and(|(expected, _)| identity(parent, true).is_ok_and(|id| id == *expected))
    });
    if matches {
        let (_, observe) = ORIGINAL_BASE_COVER.with(|hook| hook.borrow_mut().take().unwrap());
        observe(parent)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) type BaseAfterCover = fn(&File, u64, &File) -> io::Result<()>;
#[cfg(test)]
thread_local! {
    pub(super) static AFTER_BASE_COVER: std::cell::Cell<Option<BaseAfterCover>> = const {
        std::cell::Cell::new(None)
    };
}

impl PreparedEpoch {
    fn new(
        root: &File,
        backend: &File,
        writer: &File,
        number: u64,
        capture: Capture,
        predecessor: Option<Predecessor>,
        old: Option<&Inventory<'_>>,
    ) -> io::Result<Self> {
        capture.check(backend, writer)?;
        if let Some(old) = old {
            old.recheck(root)?;
        }
        fd::mkdirat(root, preparing(number).as_str(), Mode::RWXU)?;
        let directory = open(root, &preparing(number), false, true)?;
        let text = Epoch::base_text(root, &directory, number, &capture, predecessor.as_ref())?;
        if text.len() > MAX_ENTRY {
            return Err(bad("epoch base exceeds its encoded bound"));
        }
        let mut base_file = open(&directory, BASE, true, false)?;
        base_file.write_all(text.as_bytes())?;
        fd::fchmod(&base_file, Mode::RUSR)?;
        let base_stamp = Stamp::read(&base_file)?;
        let origin = preparation_origin(&directory)?;
        Ok(Self {
            epoch: Epoch {
                number,
                directory,
                base: capture,
                base_file,
                base_stamp,
                predecessor,
            },
            text,
            origin,
        })
    }

    fn recheck(&self, root: &File) -> io::Result<()> {
        if identity(
            &open(root, &preparing(self.epoch.number), false, true)?,
            true,
        )? != identity(&self.epoch.directory, true)?
            || preparation_origin(&self.epoch.directory)? != self.origin
            || Stamp::read(&self.epoch.base_file)? != self.epoch.base_stamp
            || Stamp::read(&open(&self.epoch.directory, BASE, false, false)?)?
                != self.epoch.base_stamp
        {
            return Err(bad(
                "prepared epoch lost its original named directory or base",
            ));
        }
        Ok(())
    }

    fn cover(
        self,
        root: &File,
        backend: &File,
        writer: &File,
        old: Option<&Inventory<'_>>,
    ) -> io::Result<CoveredEpoch> {
        self.recheck(root)?;
        self.epoch.base.check(backend, writer)?;
        if let Some(old) = old {
            old.recheck(root)?;
        }
        cover_frozen(
            &self.epoch.directory,
            BASE,
            &self.epoch.base_file,
            &self.text,
            &self.epoch.base_stamp,
        )?;
        #[cfg(test)]
        if let Some(observe) = AFTER_BASE_COVER.with(std::cell::Cell::take) {
            observe(root, self.epoch.number, &self.epoch.directory)?;
        }
        self.recheck(root)?;
        #[cfg(test)]
        boundary("base-covered")?;
        self.epoch.base.check(backend, writer)?;
        if let Some(old) = old {
            old.recheck(root)?;
        }
        self.recheck(root)?;
        Ok(CoveredEpoch(self))
    }
}

impl CoveredEpoch {
    fn publish(
        self,
        root: &File,
        backend: &File,
        writer: &File,
        old: Option<&Inventory<'_>>,
    ) -> io::Result<Epoch> {
        let prepared = self.0;
        prepared.recheck(root)?;
        prepared.epoch.base.check(backend, writer)?;
        if let Some(old) = old {
            old.recheck(root)?;
        }
        // Recheck the original NAME after every preceding fallible IO, immediately
        // before the owned rename. A detached held directory cannot name a replacement.
        prepared.recheck(root)?;
        let epoch = prepared.epoch;
        fd::renameat_with(
            root,
            preparing(epoch.number).as_str(),
            root,
            name(epoch.number).as_str(),
            RenameFlags::NOREPLACE,
        )?;
        aterm_durable::durable(root)?;
        #[cfg(test)]
        boundary("epoch-published")?;
        epoch.base.check(backend, writer)?;
        if let Some(old) = old {
            old.recheck(root)?;
        }
        if Stamp::read(&epoch.base_file)? != epoch.base_stamp
            || Stamp::read(&open(&epoch.directory, BASE, false, false)?)? != epoch.base_stamp
        {
            return Err(bad("epoch publication changed its original base"));
        }
        epoch.recheck(root)?;
        Ok(epoch)
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "BeginRotation",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn begin_rotation(
    root: &File,
    backend: &File,
    writer: &File,
    number: u64,
    capture: Capture,
    predecessor: Predecessor,
    old: &Inventory<'_>,
) -> io::Result<PreparedEpoch> {
    let prepared = PreparedEpoch::new(
        root,
        backend,
        writer,
        number,
        capture,
        Some(predecessor),
        Some(old),
    )?;
    #[cfg(test)]
    boundary("rotation-prepared")?;
    Ok(prepared)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "CoverBase",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn cover_rotation(
    prepared: PreparedEpoch,
    root: &File,
    backend: &File,
    writer: &File,
    old: &Inventory<'_>,
) -> io::Result<CoveredEpoch> {
    prepared.cover(root, backend, writer, Some(old))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "Publish",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn publish_rotation(
    covered: CoveredEpoch,
    root: &File,
    backend: &File,
    writer: &File,
    old: &Inventory<'_>,
) -> io::Result<Epoch> {
    covered.publish(root, backend, writer, Some(old))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "PruneOne",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn prune_member(parent: &File, name: &str, held: &File, stamp: &Stamp) -> io::Result<()> {
    if Stamp::read(held)? != *stamp || Stamp::read(&open(parent, name, false, false)?)? != *stamp {
        return Err(bad("superseded member changed before prune"));
    }
    fd::unlinkat(parent, name, fd::AtFlags::empty())?;
    #[cfg(test)]
    boundary("predecessor-unlinked")?;
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityHistory",
        action = "RemoveEmptyPredecessor",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::history_model_tests::project"
    )
)]
fn remove_empty_predecessor(root: &File, name: &str, expected: &Value) -> io::Result<()> {
    if identity(&open(root, name, false, true)?, true)? != *expected {
        return Err(bad("superseded epoch changed before removal"));
    }
    fd::unlinkat(root, name, fd::AtFlags::REMOVEDIR)?;
    aterm_durable::durable(root)
}

fn preparation_origin(directory: &File) -> io::Result<(u64, u64, u32, i64, i64, i64, i64)> {
    let before = directory_origin(directory)?;
    let mut count = 0;
    for item in fd::Dir::read_from(directory.as_fd())? {
        let item = item?;
        let child = item
            .file_name()
            .to_str()
            .map_err(|_| bad("non-UTF8 prepared epoch child"))?;
        if matches!(child, "." | "..") {
            continue;
        }
        if child != BASE || count != 0 {
            return Err(bad("unknown prepared epoch child"));
        }
        count += 1;
    }
    if count != 1 || directory_origin(directory)? != before {
        return Err(bad("prepared epoch inventory is incomplete or changed"));
    }
    Ok(before)
}

/// At most a current epoch plus one fully declared predecessor may exist. A preparation
/// or an unknown child cannot be skipped in favour of older authority.
fn epoch_numbers(root: &File, expected_stem: &str) -> io::Result<Vec<u64>> {
    let before = directory_origin(root)?;
    let mut numbers = Vec::new();
    let mut working = 0;
    for item in fd::Dir::read_from(root.as_fd())? {
        let item = item?;
        let child = item
            .file_name()
            .to_str()
            .map_err(|_| bad("non-UTF8 claim child"))?;
        if matches!(child, "." | ".." | HEADER | MUTATION | "closed.json") {
            continue;
        }
        if stem(child).is_ok_and(|s| s == expected_stem) {
            working += 1;
            if working > 1 {
                return Err(bad("claim root contains multiple working records"));
            }
            continue;
        }
        let Some(raw) = child
            .strip_prefix("epoch-")
            .and_then(|s| s.strip_suffix(".noindex"))
        else {
            return Err(bad("unknown claim child"));
        };
        let n = raw
            .parse::<u64>()
            .map_err(|_| bad("incomplete or unknown epoch publication"))?;
        if n == 0 || name(n) != child || numbers.len() >= 2 {
            return Err(bad("unbounded or noncanonical epoch history"));
        }
        numbers.push(n);
    }
    numbers.sort_unstable();
    if numbers.len() == 2 && numbers[0].checked_add(1) != Some(numbers[1]) {
        return Err(bad("gapped epoch history"));
    }
    if directory_origin(root)? != before {
        return Err(bad("claim directory changed during epoch inventory"));
    }
    Ok(numbers)
}

#[cfg(test)]
thread_local! {static INTERRUPT:std::cell::Cell<Option<&'static str>>=const {std::cell::Cell::new(None)};}

#[cfg(test)]
pub(super) fn interrupt(stage: &'static str) {
    INTERRUPT.with(|slot| slot.set(Some(stage)));
}

#[cfg(test)]
pub(super) fn take_interruption() -> Option<&'static str> {
    INTERRUPT.with(std::cell::Cell::take)
}

#[cfg(test)]
fn boundary(stage: &str) -> io::Result<()> {
    if INTERRUPT.with(|slot| {
        if slot.get() == Some(stage) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other("injected epoch transition interruption"));
    }
    Ok(())
}
