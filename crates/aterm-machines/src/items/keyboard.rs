// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-keyboard Caps Lock to Control `modifiermapping` remaps in ByHost defaults.

use super::defaults::{Target, Want, render, revert_line};
use super::{Ctx, Item};
use crate::model::{Effect, Form, PValue, Probe, Read, User};
use crate::plist::Entries;
use crate::port::System;
use serde::Deserialize;
use std::sync::{Mutex, PoisonError};

/// Caps Lock's HID usage, as the mapping stores it (30064771129).
pub const CAPS_LOCK: i64 = 0x7_0000_0039;
/// Left Control HID usage (30064771296).
pub const LEFT_CONTROL: i64 = 0x7_0000_00E0;
/// Right Control (30064771300): what System Settings writes for "Control".
pub const RIGHT_CONTROL: i64 = 0x7_0000_00E4;

const SRC: &str = "HIDKeyboardModifierMappingSrc";
const DST: &str = "HIDKeyboardModifierMappingDst";

/// What each model's ByHost global remap key starts with ([`Model::key`]).
const MAPPING_KEY: &str = "com.apple.keyboard.modifiermapping.";

/// The system domain listing the keyboard models this Mac has identified.
pub const KEYBOARD_TYPE: &str = "com.apple.keyboardtype";

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `"control"` maps Caps Lock to Control.
    #[serde(default)]
    pub caps_lock: Option<String>,
    /// Extra keyboard models as `<vendor>-<product>` decimal ids.
    #[serde(default)]
    pub models: Vec<String>,
}

/// What Caps Lock should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsLock {
    /// Act as Control.
    Control,
    /// Act as Caps Lock: no mapping.
    CapsLock,
}

/// A keyboard model, by its USB/Bluetooth ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Model {
    /// Vendor id (0 for Apple silicon's built-in keyboard).
    pub vendor: u32,
    /// Product id.
    pub product: u32,
}

impl Model {
    /// `<vendor>-<product>-0`, as ids and mapping keys spell it.
    #[must_use]
    pub fn suffix(self) -> String {
        format!("{}-{}-0", self.vendor, self.product)
    }

    /// The ByHost global key that holds this model's remaps.
    #[must_use]
    pub fn key(self) -> String {
        format!("{MAPPING_KEY}{}", self.suffix())
    }

    /// The model whose remaps the ByHost global key `key` holds ([`Self::key`]).
    #[must_use]
    pub fn of_key(key: &str) -> Option<Self> {
        Self::parse(key.strip_prefix(MAPPING_KEY)?)
    }

    /// `<vendor>-<product>` (or `<vendor>-<product>-0`), decimal.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let vendor = crate::parse_uint(parts.next()?)?;
        let product = crate::parse_uint(parts.next()?)?;
        match (parts.next(), parts.next()) {
            (None | Some("0"), None) => Some(Self { vendor, product }),
            _ => None,
        }
    }
}

/// The models `com.apple.keyboardtype` lists, from its domain's entries.
///
/// # Errors
/// One line when `keyboardtype` holds something else: what it lists is then unknown, not empty.
pub fn seen_models(entries: &Entries) -> Result<Vec<Model>, String> {
    let types = match entries.iter().find(|(k, _)| k == "keyboardtype") {
        None => return Ok(Vec::new()),
        Some((_, Err(why))) => return Err(format!("keyboardtype: {why}")),
        Some((_, Ok(PValue::Dict(types)))) => types,
        Some((_, Ok(other))) => {
            return Err(format!(
                "keyboardtype holds {other}, not a dictionary of keyboard models"
            ));
        }
    };
    let mut models = types
        .iter()
        .map(|(k, _)| {
            let parts: Vec<&str> = k.split('-').collect();
            match parts.as_slice() {
                [product, vendor, n] if crate::parse_uint::<u32>(n).is_some() => {
                    match (crate::parse_uint(product), crate::parse_uint(vendor)) {
                        (Some(product), Some(vendor)) => Some(Model { vendor, product }),
                        _ => None,
                    }
                }
                _ => None,
            }
            .ok_or_else(|| format!("keyboardtype key {k:?} is not <product>-<vendor>-<n>"))
        })
        .collect::<Result<Vec<Model>, String>>()?;
    models.sort();
    models.dedup();
    Ok(models)
}

fn usage_name(usage: i64) -> String {
    match usage {
        CAPS_LOCK => "Caps Lock".into(),
        LEFT_CONTROL => "Left Control".into(),
        RIGHT_CONTROL => "Right Control".into(),
        0x7_0000_00E1 => "Left Shift".into(),
        0x7_0000_00E2 => "Left Option".into(),
        0x7_0000_00E3 => "Left Command".into(),
        0x7_0000_00E5 => "Right Shift".into(),
        0x7_0000_00E6 => "Right Option".into(),
        0x7_0000_00E7 => "Right Command".into(),
        other => format!("{other:#x}"),
    }
}

fn int(entry: &PValue, field: &str) -> Option<i64> {
    let PValue::Dict(fields) = entry else {
        return None;
    };
    match fields.iter().find(|(k, _)| k == field) {
        Some((_, PValue::Int(i))) => Some(*i),
        _ => None,
    }
}

fn is_caps_lock(entry: &PValue) -> bool {
    int(entry, SRC) == Some(CAPS_LOCK)
}

/// The Caps Lock to Left Control entry.
#[must_use]
pub fn caps_to_control() -> PValue {
    PValue::Dict(vec![
        (SRC.into(), PValue::Int(CAPS_LOCK)),
        (DST.into(), PValue::Int(LEFT_CONTROL)),
    ])
}

fn describe(read: &Read<PValue>) -> String {
    match read {
        Read::Value(PValue::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|e| match (int(e, SRC), int(e, DST)) {
                (Some(s), Some(d)) => format!("{}→{}", usage_name(s), usage_name(d)),
                _ => e.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Read::Value(PValue::Array(_)) | Read::Absent => "no remap".into(),
        other => render(other),
    }
}

/// What the key must become for Caps Lock to do what `mode` says; `None` when it already does.
///
/// # Errors
/// Returns an error if the read failed or the key holds a non-array value.
fn plan(read: &Read<PValue>, mode: CapsLock) -> Result<Option<Want>, String> {
    let items: &[PValue] = match read {
        Read::Unusable(why) => return Err(why.clone()),
        Read::Absent => &[],
        Read::Value(PValue::Array(items)) => items,
        Read::Value(other) => {
            return Err(format!(
                "holds {other}, not an array of key mappings; left alone"
            ));
        }
    };
    let caps: Vec<&PValue> = items.iter().filter(|e| is_caps_lock(e)).collect();
    let others: Vec<PValue> = items.iter().filter(|e| !is_caps_lock(e)).cloned().collect();
    match mode {
        CapsLock::Control => {
            let done = !caps.is_empty()
                && caps
                    .iter()
                    .all(|e| matches!(int(e, DST), Some(LEFT_CONTROL | RIGHT_CONTROL)));
            if done {
                return Ok(None);
            }
            let at = items.iter().position(is_caps_lock).unwrap_or(others.len());
            let mut next = others;
            next.insert(at.min(next.len()), caps_to_control());
            Ok(Some(Want::Value(PValue::Array(next))))
        }
        CapsLock::CapsLock if caps.is_empty() => Ok(None),
        CapsLock::CapsLock if others.is_empty() => Ok(Some(Want::Unset)),
        CapsLock::CapsLock => Ok(Some(Want::Value(PValue::Array(others)))),
    }
}

const fn wanted_words(mode: CapsLock) -> &'static str {
    match mode {
        CapsLock::Control => "Caps Lock→Control",
        CapsLock::CapsLock => "Caps Lock unmapped",
    }
}

fn write(sys: &dyn System, target: &Target, want: &Want) -> Result<(), String> {
    let cmd = match want {
        Want::Value(v) => target.write_cmd(v),
        Want::Unset => target.delete_cmd(),
    };
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

/// Caps Lock on one keyboard model for one user.
#[derive(Debug)]
pub struct CapsLockKey {
    /// The model.
    pub model: Model,
    /// Why this model has an item (`built in`, `attached`, `listed in [keyboard] models`).
    pub source: &'static str,
    /// Whose ByHost global domain holds the key.
    pub user: User,
    /// What Caps Lock should do.
    pub mode: CapsLock,
    before: Mutex<Option<Read<PValue>>>,
}

impl CapsLockKey {
    fn target(&self) -> Target {
        Target::host("-g", &self.model.key(), &self.user)
    }
}

impl Item for CapsLockKey {
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        let target = self.target();
        let read = target.read(sys);
        Ok(if plan(&read, self.mode)?.is_some() {
            vec![(target, read)]
        } else {
            Vec::new()
        })
    }
    fn id(&self) -> String {
        format!(
            "keyboard.caps-lock.{}@{}",
            self.model.suffix(),
            self.user.name
        )
    }

    fn family(&self) -> &'static str {
        "keyboard"
    }

    fn title(&self) -> String {
        format!(
            "{} on keyboard {} ({}) for {}",
            wanted_words(self.mode),
            self.model.suffix(),
            self.source,
            self.user.name
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let read = self.target().read(sys);
        match plan(&read, self.mode) {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Ok {
                now: describe(&read),
            },
            Ok(Some(_)) => {
                let from = describe(&read);
                *self.before.lock().unwrap_or_else(PoisonError::into_inner) = Some(read);
                Probe::Change {
                    from,
                    to: wanted_words(self.mode).into(),
                }
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let target = self.target();
        match plan(&target.read(sys), self.mode)? {
            None => Ok(()),
            Some(want) => write(sys, &target, &want),
        }
    }

    fn revert(&self) -> Option<String> {
        let before = self
            .before
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        revert_line(&self.target(), &before, None)
    }

    fn effect(&self) -> Effect {
        Effect::Login
    }
}

/// Caps Lock on every model `com.apple.keyboardtype` lists that no other item covers, for one user.
#[derive(Debug)]
pub struct SeenKeyboards {
    /// Whose ByHost global domain holds the keys.
    pub user: User,
    /// What Caps Lock should do.
    pub mode: CapsLock,
    /// The models other items already cover.
    pub covered: Vec<Model>,
    before: Mutex<Vec<(Model, Read<PValue>)>>,
}

struct SeenPlan {
    model: Model,
    read: Read<PValue>,
    want: Option<Want>,
}

impl SeenKeyboards {
    fn plans(&self, sys: &dyn System) -> Result<Vec<SeenPlan>, String> {
        let types = Target::system(KEYBOARD_TYPE, "keyboardtype");
        let models: Vec<Model> = seen_models(&types.entries(sys)?)?
            .into_iter()
            .filter(|m| !self.covered.contains(m))
            .collect();
        if models.is_empty() {
            return Ok(Vec::new());
        }
        let global = Target::host("-g", "", &self.user);
        let entries = global.entries(sys)?;
        models
            .into_iter()
            .map(|model| {
                let key = model.key();
                let read = match entries.iter().find(|(k, _)| *k == key) {
                    None => Read::Absent,
                    Some((_, Ok(v))) => Read::Value(v.clone()),
                    Some((_, Err(why))) => Read::Unusable(why.clone()),
                };
                let want = plan(&read, self.mode).map_err(|e| format!("{key}: {e}"))?;
                Ok(SeenPlan { model, read, want })
            })
            .collect()
    }

    fn target(&self, model: Model) -> Target {
        Target::host("-g", &model.key(), &self.user)
    }
}

impl Item for SeenKeyboards {
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        Ok(self
            .plans(sys)?
            .into_iter()
            .filter(|p| p.want.is_some())
            .map(|p| (self.target(p.model), p.read))
            .collect())
    }
    fn id(&self) -> String {
        format!("keyboard.caps-lock.seen@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "keyboard"
    }

    fn title(&self) -> String {
        format!(
            "{} on the other keyboard models this Mac has seen (/Library/Preferences/{KEYBOARD_TYPE}) for {}",
            wanted_words(self.mode),
            self.user.name
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let plans = match self.plans(sys) {
            Err(why) => return Probe::Unusable { why },
            Ok(plans) => plans,
        };
        if plans.is_empty() {
            return Probe::Ok {
                now: "no other keyboard model seen".into(),
            };
        }
        let list = |rows: &[&SeenPlan]| {
            rows.iter()
                .map(|p| format!("{}: {}", p.model.suffix(), describe(&p.read)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let changing: Vec<&SeenPlan> = plans.iter().filter(|p| p.want.is_some()).collect();
        if changing.is_empty() {
            let all: Vec<&SeenPlan> = plans.iter().collect();
            return Probe::Ok { now: list(&all) };
        }
        *self.before.lock().unwrap_or_else(PoisonError::into_inner) =
            changing.iter().map(|p| (p.model, p.read.clone())).collect();
        Probe::Change {
            from: list(&changing),
            to: wanted_words(self.mode).into(),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for plan in self.plans(sys)? {
            if let Some(want) = plan.want {
                write(sys, &self.target(plan.model), &want)?;
            }
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        let before = self.before.lock().unwrap_or_else(PoisonError::into_inner);
        let lines: Vec<String> = before
            .iter()
            .filter_map(|(m, read)| revert_line(&self.target(*m), read, None))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn effect(&self) -> Effect {
        Effect::Login
    }

    /// A change is to new targets when every model it names is one the reading aterm read back
    /// does not list: macOS listed a keyboard model since, whose Caps Lock nobody set. Both
    /// readings are [`SeenKeyboards::probe`]'s: `<model>: <remap>` per model, `; ` between them
    /// (a remap's own list is `, `-separated), or a sentence that names no model.
    fn new_targets(&self, held: &str, from: &str) -> bool {
        let models = |reading: &str| -> Vec<Model> {
            reading
                .split("; ")
                .filter_map(|part| Model::parse(part.split_once(": ")?.0))
                .collect()
        };
        let held = models(held);
        let changing = models(from);
        !changing.is_empty() && changing.iter().all(|model| !held.contains(model))
    }
}

/// Expands `table` into per-keyboard items.
///
/// # Errors
/// One line naming what cannot be expanded.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let Some(mode) = table.caps_lock.as_deref() else {
        if table.models.is_empty() {
            return Ok(Vec::new());
        }
        return Err("[keyboard] models lists keyboards for caps_lock, which is not set".into());
    };
    let mode = match mode {
        "control" => CapsLock::Control,
        "caps-lock" => CapsLock::CapsLock,
        other => {
            return Err(format!(
                "[keyboard] caps_lock = {other:?}: \"control\" or \"caps-lock\""
            ));
        }
    };
    let mut models: Vec<(Model, &'static str)> = Vec::new();
    let mut add = |model: Model, source: &'static str| {
        if !models.iter().any(|(m, _)| *m == model) {
            models.push((model, source));
        }
    };
    if ctx.facts.arch == "arm64" && ctx.facts.form == Form::Laptop {
        add(
            Model {
                vendor: 0,
                product: 0,
            },
            "built in",
        );
    }
    for kb in ctx.facts.keyboards.iter().filter(|k| k.builtin) {
        add(
            Model {
                vendor: kb.vendor,
                product: kb.product,
            },
            "built in",
        );
    }
    for kb in ctx.facts.keyboards.iter().filter(|k| !k.builtin) {
        add(
            Model {
                vendor: kb.vendor,
                product: kb.product,
            },
            "attached",
        );
    }
    for text in &table.models {
        let model = Model::parse(text).ok_or_else(|| {
            format!("[keyboard] models: {text:?} is not <vendor>-<product> in decimal (1452-591)")
        })?;
        add(model, "listed in [keyboard] models");
    }
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for user in &ctx.users {
        for (model, source) in &models {
            items.push(Box::new(CapsLockKey {
                model: *model,
                source,
                user: user.clone(),
                mode,
                before: Mutex::new(None),
            }));
        }
        items.push(Box::new(SeenKeyboards {
            user: user.clone(),
            mode,
            covered: models.iter().map(|(m, _)| *m).collect(),
            before: Mutex::new(Vec::new()),
        }));
    }
    Ok(items)
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
