// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard remaps: Caps Lock as Control, per keyboard model and per user, as ByHost
//! `modifiermapping` keys that take effect at login (design §1.5, §5.1).
//!
//! macOS keeps a modifier remap per keyboard model in the user's ByHost global domain:
//! `defaults -currentHost write -g com.apple.keyboard.modifiermapping.<vendor>-<product>-0`
//! holds an array of `{HIDKeyboardModifierMappingSrc, HIDKeyboardModifierMappingDst}`
//! dictionaries (HID usages, `0x700000000 | usage`). The keys are decimal, vendor first:
//! the 27.0 VM's virtual keyboard (`hidutil list`: `0x5ac 0x1`, built in) maps as
//! `1452-1-0`, and Apple silicon's built-in keyboard as `0-0-0`.
//!
//! A model gets an item when it is the built-in keyboard, a keyboard `hidutil` saw
//! (`ctx.facts.keyboards`), or listed in `[keyboard] models`. The models this Mac has
//! seen before, in `/Library/Preferences/com.apple.keyboardtype`, are read when the
//! items probe, since nothing reads them before: one more item per user covers each of
//! those the others do not. That domain names a model PRODUCT first
//! (`keyboardtype = {"49299-1133-0" = 40}` on a 27.0.1 Mac with a Logitech keyboard,
//! vendor 1133 = 0x46d), the reverse of the mapping key; the VM's is empty.
//!
//! Only the Caps Lock entry of an array is touched: a remap of another key the user made
//! stays. Caps Lock to either Control counts as done (System Settings writes Right
//! Control, 0x7000000E4; this crate writes Left Control, 0x7000000E0, as design §1.5
//! says). `"caps-lock"` removes the Caps Lock entry, and the key with it when nothing
//! else is left.

use super::defaults::{Target, Want, render, revert_line};
use super::{Ctx, Item};
use crate::model::{Effect, Form, PValue, Probe, Read, User};
use crate::plist::Entries;
use crate::port::System;
use serde::Deserialize;
use std::sync::{Mutex, PoisonError};

/// Caps Lock's HID usage, as the mapping stores it (30064771129).
pub const CAPS_LOCK: i64 = 0x7_0000_0039;
/// Left Control (30064771296): what this crate maps Caps Lock to.
pub const LEFT_CONTROL: i64 = 0x7_0000_00E0;
/// Right Control (30064771300): what System Settings writes for "Control".
pub const RIGHT_CONTROL: i64 = 0x7_0000_00E4;

const SRC: &str = "HIDKeyboardModifierMappingSrc";
const DST: &str = "HIDKeyboardModifierMappingDst";

/// The system domain listing the keyboard models this Mac has identified.
pub const KEYBOARD_TYPE: &str = "com.apple.keyboardtype";

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `"control"` maps Caps Lock to Control; `"caps-lock"` removes the mapping; absent leaves it alone.
    #[serde(default)]
    pub caps_lock: Option<String>,
    /// Extra keyboard models as `<vendor>-<product>` decimal ids, for keyboards not attached at apply time.
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
        format!("com.apple.keyboard.modifiermapping.{}", self.suffix())
    }

    /// `<vendor>-<product>` (or `<vendor>-<product>-0`), decimal.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let vendor = parts.next()?.parse().ok()?;
        let product = parts.next()?.parse().ok()?;
        match (parts.next(), parts.next()) {
            (None | Some("0"), None) => Some(Self { vendor, product }),
            _ => None,
        }
    }
}

/// The models `com.apple.keyboardtype` lists, from its domain's entries. Its keys are
/// `<product>-<vendor>-<n>`, decimal; no `keyboardtype` key means no model was seen.
///
/// # Errors
/// One line when `keyboardtype` holds something else: what it lists is then unknown, not
/// empty.
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
                [product, vendor, n] if n.parse::<u32>().is_ok() => {
                    match (product.parse(), vendor.parse()) {
                        (Ok(product), Ok(vendor)) => Some(Model { vendor, product }),
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

/// A mapping array in words: `Caps Lock→Left Control, Left Command→Left Option`.
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

/// What the key must become for Caps Lock to do what `mode` says; `None` when it already
/// does. Every entry but Caps Lock's stays where it is.
///
/// # Errors
/// The read's reason when it was unusable, or a key holding something other than an
/// array of mappings (left alone rather than overwritten).
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

fn wanted_words(mode: CapsLock) -> &'static str {
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
        // Planned again from a fresh read, so a mapping changed since the probe is kept.
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

/// Caps Lock on every model `com.apple.keyboardtype` lists that no other item covers, for
/// one user.
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

/// One seen model: what its key holds, and what it must become (`None`: nothing).
struct SeenPlan {
    model: Model,
    read: Read<PValue>,
    want: Option<Want>,
}

impl SeenKeyboards {
    /// Each uncovered seen model, what its key holds, and what it must become.
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
}

/// The items `table` asks for: for each user, one per known keyboard model (built in,
/// attached, listed), then one for the models this Mac has seen.
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
    // Apple silicon laptops report their keyboard as vendor 0, product 0 whether or not
    // hidutil was read.
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
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, export, missing, user};
    use crate::model::{Keyboard, Mode, Outcome};
    use crate::port::{Fake, Ran};

    /// `defaults -currentHost export -g -` for the 27.0 VM's admin: it holds
    /// `com.apple.keyboard.modifiermapping.1452-1-0` = Caps Lock → Left Control.
    const VM_CURRENT_HOST_GLOBAL: &[u8] =
        include_bytes!("../../fixtures/prefs/vm-27.0-currenthost-global.plist");

    /// `defaults export /Library/Preferences/com.apple.keyboardtype -` on a 27.0.1
    /// (26A434) Mac that has seen three external keyboards (read only).
    const SEEN_KEYBOARDTYPE: &str = "<dict>\n\t<key>keyboardtype</key>\n\t<dict>\n\t\t<key>321-9456-0</key>\n\t\t<integer>40</integer>\n\t\t<key>49299-1133-0</key>\n\t\t<integer>40</integer>\n\t\t<key>8352-9456-0</key>\n\t\t<integer>40</integer>\n\t</dict>\n</dict>";

    const GLOBAL: &str = "admin$ /usr/bin/defaults -currentHost export -g -";
    const TYPES: &str = "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -";

    /// The VM's keyboards, as its `hidutil list` printed them: `0x5ac 0x1` Virtual
    /// Keyboard (built in) and `0x5ac 0x8105` Virtual USB Keyboard.
    fn vm_ctx() -> Ctx {
        let mut c = ctx(&[user("admin", 501)]);
        c.facts.form = Form::Virtual;
        c.facts.keyboards = vec![
            Keyboard {
                vendor: 0x5ac,
                product: 0x1,
                name: "Virtual Keyboard".into(),
                builtin: true,
            },
            Keyboard {
                vendor: 0x5ac,
                product: 0x8105,
                name: "Virtual USB Keyboard".into(),
                builtin: false,
            },
        ];
        c
    }

    fn table(mode: &str, models: &[&str]) -> Table {
        Table {
            caps_lock: Some(mode.into()),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    fn vm_global() -> Ran {
        Ran {
            status: Some(0),
            stdout: VM_CURRENT_HOST_GLOBAL.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn the_vm_gets_one_item_per_keyboard_hidutil_saw_and_one_for_the_seen_models() {
        let items = expand(&table("control", &["1133-49299", "1452-1-0"]), &vm_ctx()).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "keyboard.caps-lock.1452-1-0@admin",
                "keyboard.caps-lock.1452-33029-0@admin",
                "keyboard.caps-lock.1133-49299-0@admin",
                "keyboard.caps-lock.seen@admin",
            ]
        );
        assert_eq!(
            items[0].title(),
            "Caps Lock→Control on keyboard 1452-1-0 (built in) for admin"
        );
        assert!(items.iter().all(|i| i.effect() == Effect::Login));
    }

    #[test]
    fn an_apple_silicon_laptop_maps_its_built_in_keyboard_as_0_0_0() {
        let mut c = ctx(&[user("alice", 501), user("bob", 502)]);
        c.facts.form = Form::Laptop;
        let items = expand(&table("control", &[]), &c).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "keyboard.caps-lock.0-0-0@alice",
                "keyboard.caps-lock.seen@alice",
                "keyboard.caps-lock.0-0-0@bob",
                "keyboard.caps-lock.seen@bob",
            ]
        );
        // An Intel laptop's built-in keyboard has real ids, so it comes from hidutil.
        c.facts.arch = "x86_64".into();
        let items = expand(&table("control", &[]), &c).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn a_table_that_does_not_say_what_caps_lock_does_is_refused_or_empty() {
        assert!(expand(&Table::default(), &vm_ctx()).unwrap().is_empty());
        let err = expand(
            &Table {
                caps_lock: None,
                models: vec!["1-2".into()],
            },
            &vm_ctx(),
        )
        .err()
        .unwrap();
        assert!(err.contains("caps_lock"), "{err}");
        let err = expand(&table("ctrl", &[]), &vm_ctx()).err().unwrap();
        assert!(err.contains("\"control\" or \"caps-lock\""), "{err}");
        for bad in ["0x5ac-0x1", "1452", "1452-1-1", "a-b", "1-2-0-0"] {
            let err = expand(&table("control", &[bad]), &vm_ctx()).err().unwrap();
            assert!(err.contains("not <vendor>-<product>"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_vms_mapped_keyboard_probes_ok_and_its_unmapped_one_is_written() {
        let sys = Fake::root();
        sys.on(GLOBAL, vm_global());
        let items = expand(&table("control", &[]), &vm_ctx()).unwrap();
        let c = vm_ctx();
        assert_eq!(
            items[0].probe(&sys, &c),
            Probe::Ok {
                now: "Caps Lock→Left Control".into()
            }
        );
        assert_eq!(
            items[1].probe(&sys, &c),
            Probe::Change {
                from: "no remap".into(),
                to: "Caps Lock→Control".into()
            }
        );

        let sys = Fake::root();
        let after = String::from_utf8_lossy(VM_CURRENT_HOST_GLOBAL).replace(
            "\t<key>com.apple.trackpad.enableSecondaryClick</key>",
            "\t<key>com.apple.keyboard.modifiermapping.1452-33029-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.trackpad.enableSecondaryClick</key>",
        );
        sys.on(GLOBAL, vm_global());
        sys.on(GLOBAL, vm_global());
        sys.on(GLOBAL, Ran::ok(&after));
        sys.on(
            "admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.1452-33029-0 '<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>'",
            Ran::ok(""),
        );
        let one: Vec<Box<dyn Item>> = items.into_iter().skip(1).take(1).collect();
        let rows = engine::run(&one, Mode::Apply, &sys, &c, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "admin$ /usr/bin/defaults -currentHost delete -g com.apple.keyboard.modifiermapping.1452-33029-0  # it was not set before this run"
            )
        );
    }

    /// One entry, keys in the order `defaults export` sorts them.
    fn entry(src: i64, dst: i64) -> PValue {
        PValue::Dict(vec![
            (DST.into(), PValue::Int(dst)),
            (SRC.into(), PValue::Int(src)),
        ])
    }

    fn mapping(pairs: &[(i64, i64)]) -> PValue {
        PValue::Array(pairs.iter().map(|(s, d)| entry(*s, *d)).collect())
    }

    const LEFT_COMMAND: i64 = 0x7_0000_00E3;
    const LEFT_OPTION: i64 = 0x7_0000_00E2;

    #[test]
    fn caps_lock_to_the_right_control_system_settings_writes_counts_as_done() {
        // The 27.0.1 Mac's own 0-0-0 entry, set in System Settings.
        let read = Read::Value(mapping(&[(CAPS_LOCK, RIGHT_CONTROL)]));
        assert_eq!(plan(&read, CapsLock::Control), Ok(None));
        assert_eq!(describe(&read), "Caps Lock→Right Control");
    }

    #[test]
    fn another_remap_the_user_made_is_kept_when_caps_lock_is_mapped_or_unmapped() {
        let read = Read::Value(mapping(&[(LEFT_COMMAND, LEFT_OPTION)]));
        assert_eq!(
            plan(&read, CapsLock::Control),
            Ok(Some(Want::Value(PValue::Array(vec![
                entry(LEFT_COMMAND, LEFT_OPTION),
                caps_to_control(),
            ]))))
        );
        // Caps Lock to Escape is replaced in place; the other entry stays.
        let read = Read::Value(mapping(&[
            (CAPS_LOCK, 0x7_0000_0029),
            (LEFT_COMMAND, LEFT_OPTION),
        ]));
        let Ok(Some(Want::Value(PValue::Array(next)))) = plan(&read, CapsLock::Control) else {
            panic!("no plan");
        };
        assert_eq!(next[0], caps_to_control());
        assert_eq!(int(&next[1], SRC), Some(LEFT_COMMAND));
        // Unmapping keeps the other entry, and deletes the key when nothing is left.
        let read = Read::Value(mapping(&[
            (CAPS_LOCK, LEFT_CONTROL),
            (LEFT_COMMAND, LEFT_OPTION),
        ]));
        assert_eq!(
            plan(&read, CapsLock::CapsLock),
            Ok(Some(Want::Value(mapping(&[(LEFT_COMMAND, LEFT_OPTION)]))))
        );
        let read = Read::Value(mapping(&[(CAPS_LOCK, LEFT_CONTROL)]));
        assert_eq!(plan(&read, CapsLock::CapsLock), Ok(Some(Want::Unset)));
        assert_eq!(plan(&Read::Absent, CapsLock::CapsLock), Ok(None));
    }

    #[test]
    fn caps_lock_back_on_the_vm_deletes_its_only_mapping() {
        let sys = Fake::root();
        sys.on(GLOBAL, vm_global());
        sys.on(GLOBAL, vm_global());
        sys.on(GLOBAL, missing());
        sys.on(
            "admin$ /usr/bin/defaults -currentHost delete -g com.apple.keyboard.modifiermapping.1452-1-0",
            Ran::ok(""),
        );
        let c = vm_ctx();
        let items = expand(&table("caps-lock", &[]), &c).unwrap();
        let one: Vec<Box<dyn Item>> = items.into_iter().take(1).collect();
        let rows = engine::run(&one, Mode::Apply, &sys, &c, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(rows[0].from.as_deref(), Some("Caps Lock→Left Control"));
        assert_eq!(rows[0].to.as_deref(), Some("Caps Lock unmapped"));
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.1452-1-0 '<array><dict><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer></dict></array>'  # restores the value before this run"
            )
        );
    }

    #[test]
    fn a_key_holding_something_other_than_mappings_is_left_alone_as_unusable() {
        let read = Read::Value(PValue::Str("x".into()));
        let err = plan(&read, CapsLock::Control).unwrap_err();
        assert!(err.contains("left alone"), "{err}");
    }

    #[test]
    fn keyboardtype_names_a_model_product_first() {
        let entries =
            crate::plist::parse_domain(export(SEEN_KEYBOARDTYPE).out().as_bytes()).unwrap();
        assert_eq!(
            seen_models(&entries).unwrap(),
            [
                Model {
                    vendor: 1133,
                    product: 49299
                },
                Model {
                    vendor: 9456,
                    product: 321
                },
                Model {
                    vendor: 9456,
                    product: 8352
                },
            ]
        );
        assert!(seen_models(&Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn a_keyboardtype_domain_it_cannot_read_is_unusable_never_no_keyboards() {
        let c = vm_ctx();
        let items = expand(&table("control", &[]), &c).unwrap();
        let seen = items.iter().find(|i| i.id().contains("seen")).unwrap();
        for (body, needle) in [
            (
                "<dict>\n\t<key>keyboardtype</key>\n\t<string>x</string>\n</dict>",
                "not a dictionary of keyboard models",
            ),
            (
                "<dict>\n\t<key>keyboardtype</key>\n\t<dict>\n\t\t<key>Logitech</key>\n\t\t<integer>40</integer>\n\t</dict>\n</dict>",
                "is not <product>-<vendor>-<n>",
            ),
        ] {
            let sys = Fake::root();
            sys.on(TYPES, export(body));
            match seen.probe(&sys, &c) {
                Probe::Unusable { why } => assert!(why.contains(needle), "{why}"),
                other => panic!("{needle}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_seen_models_item_maps_each_seen_model_the_others_do_not_cover() {
        let c = vm_ctx();
        let items = expand(&table("control", &["9456-321"]), &c).unwrap();
        let seen: Vec<Box<dyn Item>> = items
            .into_iter()
            .filter(|i| i.id().contains("seen"))
            .collect();
        let sys = Fake::root();
        sys.on(TYPES, export(SEEN_KEYBOARDTYPE));
        sys.on(GLOBAL, vm_global());
        sys.on(GLOBAL, vm_global());
        let mapped = String::from_utf8_lossy(VM_CURRENT_HOST_GLOBAL).replace(
            "\t<key>com.apple.trackpad.enableSecondaryClick</key>",
            "\t<key>com.apple.keyboard.modifiermapping.1133-49299-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.keyboard.modifiermapping.9456-8352-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.trackpad.enableSecondaryClick</key>",
        );
        sys.on(GLOBAL, Ran::ok(&mapped));
        for model in ["1133-49299-0", "9456-8352-0"] {
            sys.on(
                &format!("admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.{model} '<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>'"),
                Ran::ok(""),
            );
        }
        let rows = engine::run(&seen, Mode::Apply, &sys, &c, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(
            rows[0].from.as_deref(),
            Some("1133-49299-0: no remap; 9456-8352-0: no remap")
        );
        let revert = rows[0].revert.clone().unwrap();
        assert!(
            revert.contains("delete -g com.apple.keyboard.modifiermapping.1133-49299-0"),
            "{revert}"
        );
        assert_eq!(revert.lines().count(), 2, "{revert}");
        // 9456-321 is listed in [keyboard] models, so its own item covers it.
        assert!(!sys.calls().iter().any(|c| c.contains("9456-321")));
    }

    #[test]
    fn a_mac_that_has_seen_no_other_keyboard_probes_ok() {
        let sys = Fake::root();
        // The 27.0 VM's keyboardtype domain does not exist.
        sys.on(TYPES, missing());
        let c = vm_ctx();
        let items = expand(&table("control", &[]), &c).unwrap();
        let seen = items.iter().find(|i| i.id().contains("seen")).unwrap();
        assert_eq!(
            seen.probe(&sys, &c),
            Probe::Ok {
                now: "no other keyboard model seen".into()
            }
        );
        assert_eq!(sys.calls(), [TYPES]);
    }
}
