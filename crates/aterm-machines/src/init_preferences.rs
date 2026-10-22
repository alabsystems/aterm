// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local `aterm init` desktop preferences. Persistent modifier mappings use the
//! same items as machine baselines; a separate HID item updates attached keyboards now.
//! No login agent or background process is installed.
//!
//! The widget, desktop and Dock items of one pass share one [`DesktopActivation`]: each
//! writes its keys, and the Dock, Finder and Notification Center that read them relaunch
//! once, after every write, before any of the three is read back. A Dock relaunched for
//! the desktop keys a second before the Dock's own write wrote the pins it had just read
//! back over that write, on about 187 unattended passes in a row (2026-10-21).

use crate::items::defaults::{self, Target};
use crate::items::{Activation, Ctx, Item, keyboard};
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::plist::Entries;
use crate::port::{Cmd, System};
use aterm_types::macos_setup::{Selection, Setting};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const SRC: &str = "HIDKeyboardModifierMappingSrc";
const WINDOW_MANAGER: &str = "com.apple.WindowManager";
const NOTIFICATION_CENTER: &str = "com.apple.notificationcenterui";
const FINDER: &str = "com.apple.finder";
const DOCK: &str = "com.apple.dock";
const DST: &str = "HIDKeyboardModifierMappingDst";
const HIDUTIL: &str = "/usr/bin/hidutil";
const DOCK_APPS: [(&str, &str, &str); 3] = [
    ("/Applications/aterm.app", "com.aterm.aterm", "aterm"),
    (
        "/System/Applications/System Settings.app",
        "com.apple.systempreferences",
        "System Settings",
    ),
    ("/Applications/Safari.app", "com.apple.Safari", "Safari"),
];

/// The preferred keyboard, widget and Dock settings for the resolved local user.
///
/// # Errors
/// An absent or ambiguous user, or an invalid keyboard model.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    selected_items(ctx, Selection::all())
}

/// Build only the selected preferences, before reading any preference domain.
///
/// # Errors
/// An absent or ambiguous user, or an invalid selected keyboard model.
pub fn selected_items(ctx: &Ctx, selected: Selection) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init preferences require exactly one local user".into());
    };
    let mut items = Vec::new();
    if selected.contains(Setting::CapsLock) {
        items.extend(keyboard::expand(
            &keyboard::Table {
                caps_lock: Some("control".into()),
                // Apple silicon's internal keyboard uses this model even when no HID
                // service currently advertises it. It is harmless on desktop Macs.
                models: if ctx.facts.arch == "arm64" {
                    vec!["0-0".into()]
                } else {
                    Vec::new()
                },
            },
            ctx,
        )?);
        items.extend(attached_keyboard_items(ctx)?);
    }
    // One relaunch of what the selected desktop items' writes are read by, after them all.
    let activation = Arc::new(DesktopActivation::new(user));
    for (setting, kind) in [
        (Setting::Widgets, DesktopKind::Widgets),
        (Setting::Desktop, DesktopKind::Desktop),
        (Setting::Dock, DesktopKind::Dock),
    ] {
        if selected.contains(setting) {
            items.push(Box::new(Desktop::new(user, kind, &activation)));
        }
    }
    Ok(items)
}

/// Apply Caps Lock as Control to attached HID services without replacing the
/// persistent modifier preferences. Doctor uses this after its per-key backup.
///
/// # Errors
/// The setup did not resolve exactly one local user.
pub fn attached_keyboard_items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("input activation requires exactly one local user".into());
    };
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    let mut models: Vec<keyboard::Model> = ctx
        .facts
        .keyboards
        .iter()
        .map(|k| keyboard::Model {
            vendor: k.vendor,
            product: k.product,
        })
        .collect();
    models.sort();
    models.dedup();
    for model in models {
        items.push(Box::new(AttachedKeyboard {
            model,
            user: user.clone(),
        }));
    }
    Ok(items)
}

fn run(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", cmd.display(), ran.describe()));
    }
    Ok(ran.out())
}

#[derive(Debug)]
struct AttachedKeyboard {
    model: keyboard::Model,
    user: User,
}

impl AttachedKeyboard {
    fn command(&self) -> Cmd {
        Cmd::new(HIDUTIL)
            .args([
                "property",
                "--matching",
                &format!(
                    "{{\"PrimaryUsagePage\":1,\"PrimaryUsage\":6,\"VendorID\":{},\"ProductID\":{}}}",
                    self.model.vendor, self.model.product
                ),
            ])
            .as_user(&self.user)
    }

    fn mappings(&self, sys: &dyn System) -> Result<Vec<Vec<(i64, i64)>>, String> {
        let command = self.command().args(["--get", "UserKeyMapping"]);
        // A diagnostic tail can start at a complete later service row and silently
        // omit the earlier services' unrelated maps. Only a complete record is safe
        // to merge and apply to every service matched by this model.
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|e| e.to_string())?;
        if !ran.success() {
            return Err(format!("{}: {}", command.display(), ran.describe()));
        }
        parse_hid_mappings(&ran.out())
    }
}

impl Item for AttachedKeyboard {
    fn id(&self) -> String {
        format!(
            "keyboard.attached.{}@{}",
            self.model.suffix(),
            self.user.name
        )
    }

    fn family(&self) -> &'static str {
        "keyboard"
    }

    fn title(&self) -> String {
        format!(
            "Caps Lock acts as Control now on keyboard {}",
            self.model.suffix()
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.mappings(sys).and_then(|maps| merged_mapping(&maps)) {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Ok {
                now: "Caps Lock acts as Control on every attached service".into(),
            },
            Ok(Some(_)) => Probe::Change {
                from: "Caps Lock is not Control on every attached service".into(),
                to: "Caps Lock acts as Control immediately".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let maps = self.mappings(sys)?;
        if let Some(want) = merged_mapping(&maps)? {
            let values: Vec<aterm_json::Value> = want
                .into_iter()
                .map(|(src, dst)| aterm_json::json!({SRC: src, DST: dst}))
                .collect();
            let value = aterm_json::to_string(&aterm_json::json!({"UserKeyMapping": values}))
                .map_err(|e| e.to_string())?;
            run(sys, &self.command().args(["--set", &value]))?;
        }
        Ok(())
    }
}

/// Model matching may return several HID services. Do not copy a service's unrelated
/// remaps over another service with a different map: refuse that ambiguous case.
fn merged_mapping(maps: &[Vec<(i64, i64)>]) -> Result<Option<Vec<(i64, i64)>>, String> {
    if maps.is_empty() {
        return Err(
            "the attached keyboard has no HID service; reconnect it and run aterm init again"
                .into(),
        );
    }
    let correct = |map: &Vec<(i64, i64)>| {
        let caps: Vec<_> = map
            .iter()
            .filter(|(s, _)| *s == keyboard::CAPS_LOCK)
            .collect();
        !caps.is_empty()
            && caps
                .iter()
                .all(|(_, d)| matches!(*d, keyboard::LEFT_CONTROL | keyboard::RIGHT_CONTROL))
    };
    if maps.iter().all(correct) {
        return Ok(None);
    }
    let others = |map: &Vec<(i64, i64)>| {
        map.iter()
            .copied()
            .filter(|(s, _)| *s != keyboard::CAPS_LOCK)
            .collect::<Vec<_>>()
    };
    let mut want = others(&maps[0]);
    if maps.iter().skip(1).any(|map| others(map) != want) {
        return Err("keyboard services have different unrelated remaps; left untouched".into());
    }
    want.push((keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL));
    Ok(Some(want))
}

/// `hidutil property --get` prints a table of RegistryIDs followed by Foundation's
/// integer mapping arrays, or the array alone on older macOS versions.
fn parse_hid_mappings(text: &str) -> Result<Vec<Vec<(i64, i64)>>, String> {
    let mut values = Vec::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("RegistryID") {
            continue;
        }
        if let Some((id, rest)) = line.split_once(char::is_whitespace)
            && !id.is_empty()
            && id.chars().all(|c| c.is_ascii_hexdigit())
            && let Some(rest) = rest.trim().strip_prefix("UserKeyMapping")
        {
            if !current.is_empty() {
                values.push(parse_mapping(&current)?);
                current.clear();
            }
            current.push_str(rest.trim());
        } else {
            current.push(' ');
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        values.push(parse_mapping(&current)?);
    }
    Ok(values)
}

fn parse_mapping(value: &str) -> Result<Vec<(i64, i64)>, String> {
    if value.trim() == "(null)" {
        return Ok(Vec::new());
    }
    let mut tokens = Vec::new();
    let mut start = 0;
    for (at, ch) in value.char_indices() {
        if ch.is_ascii_whitespace() || "(){},;=".contains(ch) {
            if start < at {
                tokens.push(&value[start..at]);
            }
            if !ch.is_ascii_whitespace() {
                tokens.push(&value[at..at + ch.len_utf8()]);
            }
            start = at + ch.len_utf8();
        }
    }
    if start < value.len() {
        tokens.push(&value[start..]);
    }
    let invalid =
        || "unrecognized hidutil UserKeyMapping; existing remaps left untouched".to_owned();
    if tokens.first() != Some(&"(") || tokens.last() != Some(&")") {
        return Err(invalid());
    }
    let mut at = 1;
    let mut mappings = Vec::new();
    while at < tokens.len() - 1 {
        if tokens.get(at) != Some(&"{") {
            return Err(invalid());
        }
        at += 1;
        let (mut src, mut dst) = (None, None);
        for _ in 0..2 {
            let field = *tokens.get(at).ok_or_else(invalid)?;
            if tokens.get(at + 1) != Some(&"=") || tokens.get(at + 3) != Some(&";") {
                return Err(invalid());
            }
            let number = tokens
                .get(at + 2)
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?;
            match field {
                SRC if src.is_none() => src = Some(number),
                DST if dst.is_none() => dst = Some(number),
                _ => return Err(invalid()),
            }
            at += 4;
        }
        if tokens.get(at) != Some(&"}") {
            return Err(invalid());
        }
        mappings.push((src.ok_or_else(invalid)?, dst.ok_or_else(invalid)?));
        at += 1;
        if tokens.get(at) == Some(&",") {
            at += 1;
        } else if at != tokens.len() - 1 {
            return Err(invalid());
        }
    }
    Ok(mappings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopKind {
    Widgets,
    Desktop,
    Dock,
}

/// What a desktop key should hold.
#[derive(Debug, Clone, Copy)]
enum Wanted {
    Bool(bool),
    /// An empty array: no stack, no recent app.
    Empty,
    /// The [`DOCK_APPS`] pins, as the Dock rewrites them ([`same_apps`]).
    DockApps,
    /// Notification Center's widget list with no widget ([`cleared_widgets`]).
    NoWidgets,
}

impl DesktopKind {
    /// The family's marker and journal name.
    const fn family(self) -> &'static str {
        match self {
            Self::Widgets => "widgets",
            Self::Desktop => "desktop",
            Self::Dock => "dock",
        }
    }

    /// The id of `user`'s item of this family.
    fn id(self, user: &User) -> String {
        format!("init.{}@{}", self.family(), user.name)
    }

    /// The processes that read the family's keys, in relaunch order: those of each domain
    /// its keys are in ([`crate::init_activation::readers`]).
    fn readers(self) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| {
                self.keys()
                    .iter()
                    .any(|(domain, _, _)| crate::init_activation::readers(domain).contains(process))
            })
            .collect()
    }

    /// Every key the family writes, in the order it reads them, and what each should hold:
    /// what its item changes and what its activation waits to read the same.
    const fn keys(self) -> &'static [(&'static str, &'static str, Wanted)] {
        match self {
            Self::Widgets => &[
                (WINDOW_MANAGER, "StandardHideWidgets", Wanted::Bool(true)),
                (
                    WINDOW_MANAGER,
                    "StageManagerHideWidgets",
                    Wanted::Bool(true),
                ),
                (NOTIFICATION_CENTER, "widgets", Wanted::NoWidgets),
            ],
            Self::Desktop => &[
                (
                    WINDOW_MANAGER,
                    "EnableStandardClickToShowDesktop",
                    Wanted::Bool(false),
                ),
                (
                    WINDOW_MANAGER,
                    "StandardHideDesktopIcons",
                    Wanted::Bool(false),
                ),
                (
                    WINDOW_MANAGER,
                    "StageManagerHideDesktopIcons",
                    Wanted::Bool(false),
                ),
                (FINDER, "CreateDesktop", Wanted::Bool(true)),
            ],
            Self::Dock => &[
                (DOCK, "persistent-apps", Wanted::DockApps),
                (DOCK, "persistent-others", Wanted::Empty),
                (DOCK, "static-apps", Wanted::Empty),
                (DOCK, "static-others", Wanted::Empty),
                (DOCK, "recent-apps", Wanted::Empty),
                (DOCK, "show-recents", Wanted::Bool(false)),
                (DOCK, "static-only", Wanted::Bool(false)),
                (DOCK, "minimize-to-application", Wanted::Bool(true)),
            ],
        }
    }
}

#[derive(Debug)]
struct Desktop {
    user: User,
    kind: DesktopKind,
    before: Mutex<Vec<(Target, Read<PValue>)>>,
    /// The pass's one relaunch of what the desktop items write ([`DesktopActivation`]).
    activation: Arc<DesktopActivation>,
}

type Changes = Vec<(Target, Read<PValue>, PValue)>;

impl Desktop {
    fn new(user: &User, kind: DesktopKind, activation: &Arc<DesktopActivation>) -> Self {
        Self {
            user: user.clone(),
            kind,
            before: Mutex::new(Vec::new()),
            activation: Arc::clone(activation),
        }
    }

    fn target(&self, domain: &str, key: &str) -> Target {
        Target::user(domain, key, &self.user)
    }

    fn changes(&self, sys: &dyn System) -> Result<Changes, String> {
        if self.kind == DesktopKind::Dock {
            dock_apps_available(sys, &self.user)?;
        }
        let mut changes = Vec::new();
        for &(domain, key, wanted) in self.kind.keys() {
            let target = self.target(domain, key);
            let read = target.read(sys);
            let want = match wanted {
                Wanted::NoWidgets => cleared_widgets(sys, &self.user, &read)?,
                Wanted::DockApps => {
                    unmet(&read, dock_apps(), |now, want| same_apps(sys, now, want))?
                }
                Wanted::Empty => unmet(&read, PValue::Array(Vec::new()), equivalent)?,
                Wanted::Bool(value) => unmet(&read, PValue::Bool(value), equivalent)?,
            };
            if let Some(want) = want {
                changes.push((target, read, want));
            }
        }
        Ok(changes)
    }
}

/// `want`, unless `read` already holds it by `same`.
fn unmet(
    read: &Read<PValue>,
    want: PValue,
    same: impl Fn(&PValue, &PValue) -> bool,
) -> Result<Option<PValue>, String> {
    match read {
        Read::Unusable(why) => Err(why.clone()),
        Read::Value(now) if same(now, &want) => Ok(None),
        Read::Absent | Read::Value(_) => Ok(Some(want)),
    }
}

/// The one relaunch, per pass, of the desktop processes that read what that pass's
/// widget, desktop and Dock items wrote: the Dock, the Finder and Notification Center each
/// relaunch once, after the last of those writes, and the items are read back only once
/// each is up again and past its start ([`crate::init_activation::relaunch_settled`]).
/// Each family settles alone: it is activated once every process that reads its keys
/// ([`DesktopKind::readers`]) is, so a reader that does not start again fails only the
/// families it reads, and the others are read back (and their changes kept).
/// Each family's durable marker (written before its first write) is cleared only after
/// that, so a pass that ends between the writes and the relaunch relaunches on the next.
#[derive(Debug)]
struct DesktopActivation {
    user: User,
    /// The families whose items started writing in this pass, in run order.
    owed: Mutex<Vec<DesktopKind>>,
}

/// The Dock's keys whose tiles it gives a GUID as it loads them ([`dock_apps`] writes none).
const TILE_KEYS: [&str; 2] = ["persistent-apps", "persistent-others"];

impl DesktopActivation {
    fn new(user: &User) -> Self {
        Self {
            user: user.clone(),
            owed: Mutex::new(Vec::new()),
        }
    }

    /// `kind`'s marker is written and its writes begin: this pass relaunches its readers.
    fn owe(&self, kind: DesktopKind) {
        let mut owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
        if !owed.contains(&kind) {
            owed.push(kind);
        }
    }

    /// The families this pass owes a relaunch whose marker is still pending.
    fn families(&self, sys: &dyn System) -> Result<Vec<DesktopKind>, String> {
        let owed = self
            .owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut families = Vec::new();
        for kind in owed {
            if crate::init_activation::pending(sys, &self.user, kind.family())? {
                families.push(kind);
            }
        }
        Ok(families)
    }

    /// The processes that read `families`' keys, each once, in relaunch order.
    fn consumers(families: &[DesktopKind]) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| families.iter().any(|kind| kind.readers().contains(process)))
            .collect()
    }

    /// What `process` reads of what `families` wrote, as it now holds it, each domain read
    /// once; `Err` while it is the Dock and has not yet loaded the tiles it was handed: a
    /// Dock gives each tile it loads a GUID, and a write of [`dock_apps`] carries none.
    fn look(
        &self,
        sys: &dyn System,
        families: &[DesktopKind],
        process: &str,
    ) -> Result<Vec<Result<Entries, String>>, String> {
        let keys: Vec<(&str, &str)> = families
            .iter()
            .flat_map(|kind| kind.keys().iter())
            .filter(|(domain, _, _)| crate::init_activation::readers(domain).contains(&process))
            .map(|&(domain, key, _)| (domain, key))
            .collect();
        let mut domains: Vec<&str> = Vec::new();
        for &(domain, _) in &keys {
            if !domains.contains(&domain) {
                domains.push(domain);
            }
        }
        let mut seen = Vec::with_capacity(domains.len());
        for domain in domains {
            let entries =
                Target::user(domain, "", &self.user)
                    .entries(sys)
                    .map(|entries| -> Entries {
                        entries
                            .into_iter()
                            .filter(|(key, _)| keys.contains(&(domain, key.as_str())))
                            .collect()
                    });
            if domain == DOCK
                && let Ok(entries) = &entries
                && entries.iter().any(|(key, value)| {
                    TILE_KEYS.contains(&key.as_str())
                        && value.as_ref().is_ok_and(|tiles| !loaded(tiles))
                })
            {
                return Err("the Dock has not yet loaded the tiles it was given".into());
            }
            seen.push(entries);
        }
        Ok(seen)
    }
}

/// Whether every tile of `tiles` carries the GUID a Dock gives a tile it loads.
fn loaded(tiles: &PValue) -> bool {
    let PValue::Array(tiles) = tiles else {
        return true;
    };
    tiles.iter().all(|tile| field(tile, "GUID").is_some())
}

/// `a`, `a and b`, `a, b and c`.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl Activation for DesktopActivation {
    fn id(&self) -> String {
        format!("init.relaunch@{}", self.user.name)
    }

    fn pending(&self, sys: &dyn System, _ctx: &Ctx) -> Result<Option<(String, String)>, String> {
        let families = self.families(sys)?;
        if families.is_empty() {
            return Ok(None);
        }
        let processes: Vec<String> = Self::consumers(&families)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let writers: Vec<String> = families.iter().map(|kind| kind.id(&self.user)).collect();
        let one = processes.len() == 1;
        Ok(Some((
            format!(
                "{} running with the preferences {} read at {} start",
                listed(&processes),
                if one { "it" } else { "they" },
                if one { "its" } else { "their" },
            ),
            format!(
                "{} relaunched once, after {} wrote, and up again",
                listed(&processes),
                listed(&writers),
            ),
        )))
    }

    /// Relaunch the readers of every family owed, once each, and wait for each; then
    /// clear the marker of each family all of whose readers are up, and name the items of
    /// the others, each with why its readers are not.
    fn activate(&self, sys: &dyn System, _ctx: &Ctx) -> Result<Vec<(String, String)>, String> {
        let families = self.families(sys)?;
        let processes = Self::consumers(&families);
        let answers =
            crate::init_activation::relaunch_settled(sys, &self.user, &processes, |process| {
                self.look(sys, &families, process)
            });
        let mut failed = Vec::new();
        for kind in &families {
            let why: Vec<&str> = processes
                .iter()
                .zip(&answers)
                .filter(|(process, _)| kind.readers().contains(process))
                .filter_map(|(_, answer)| answer.as_ref().err().map(String::as_str))
                .collect();
            let finished = if why.is_empty() {
                crate::init_activation::finish(sys, &self.user, kind.family())
            } else {
                Err(why.join("; "))
            };
            if let Err(why) = finished {
                failed.push((kind.id(&self.user), why));
            }
        }
        self.owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|kind| !families.contains(kind));
        Ok(failed)
    }
}

impl Item for Desktop {
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        Ok(self
            .changes(sys)?
            .into_iter()
            .map(|(target, read, _)| (target, read))
            .collect())
    }
    fn id(&self) -> String {
        self.kind.id(&self.user)
    }

    fn family(&self) -> &'static str {
        self.kind.family()
    }

    fn title(&self) -> String {
        match self.kind {
            DesktopKind::Widgets => {
                "No widgets on the desktop, in Stage Manager or Notification Center".into()
            }
            DesktopKind::Desktop => {
                "Desktop icons visible; clicking wallpaper keeps windows in place".into()
            }
            DesktopKind::Dock => {
                "Dock pins: aterm, System Settings, Safari; no stacks or recent apps".into()
            }
        }
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let pending = match crate::init_activation::pending(sys, &self.user, self.family()) {
            Ok(pending) => pending,
            Err(why) => return Probe::Unusable { why },
        };
        match self.changes(sys) {
            Err(why) => Probe::Unusable { why },
            Ok(changes) if changes.is_empty() && !pending => Probe::Ok { now: self.title() },
            Ok(changes) => {
                let from = if changes.is_empty() {
                    "stored preferences await activation after an earlier write".into()
                } else {
                    changes
                        .iter()
                        .map(|(target, _, _)| target.describe())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                *self.before.lock().unwrap_or_else(PoisonError::into_inner) = changes
                    .into_iter()
                    .map(|(target, read, _)| (target, read))
                    .collect();
                if ctx.disrupt {
                    Probe::Change {
                        from,
                        to: self.title(),
                    }
                } else {
                    Probe::Deferred { why: "relaunches the user's desktop process; use the local init command at the console".into() }
                }
            }
        }
    }

    /// Write the family's keys, after its durable marker. Its readers relaunch in the
    /// pass's one [`DesktopActivation`], after every desktop item's writes, which the
    /// engine runs before it reads this item back.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let changes = self.changes(sys)?;
        let pending = crate::init_activation::pending(sys, &self.user, self.family())?;
        if changes.is_empty() && !pending {
            return Ok(());
        }
        crate::init_activation::begin(sys, &self.user, self.family())?;
        // Owed from here: a write below that fails part way has still been made.
        self.activation.owe(self.kind);
        for (target, _, want) in changes {
            run(sys, &target.write_cmd(&want))?;
        }
        Ok(())
    }

    fn activation(&self) -> Option<Arc<dyn Activation>> {
        Some(Arc::clone(&self.activation) as Arc<dyn Activation>)
    }

    fn effect(&self) -> Effect {
        Effect::Relaunch
    }

    fn revert(&self) -> Option<String> {
        let before = self.before.lock().unwrap_or_else(PoisonError::into_inner);
        let lines: Vec<_> = before
            .iter()
            .filter_map(|(target, read)| defaults::revert_line(target, read, None))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

fn equivalent(now: &PValue, want: &PValue) -> bool {
    now.same(want)
}

fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(entries) = value else {
        return None;
    };
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn cleared_widgets(
    sys: &dyn System,
    user: &User,
    read: &Read<PValue>,
) -> Result<Option<PValue>, String> {
    let fields = match read {
        Read::Unusable(why) => return Err(why.clone()),
        // An explicit empty list also covers a fresh account whose Notification
        // Center has not yet populated its default widgets.
        Read::Absent => {
            return Ok(Some(PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ])));
        }
        Read::Value(PValue::Dict(fields)) => fields,
        Read::Value(_) => {
            return Err("widget configuration is not a dictionary; left untouched".into());
        }
    };
    let instances = fields.iter().find(|(k, _)| k == "instances");
    match instances {
        Some((_, PValue::Array(values))) if values.is_empty() => {
            match fields
                .iter()
                .find(|(key, _)| key == "DesktopWidgetPlacementStorage")
            {
                None => return Ok(None),
                Some((_, value)) if empty_widget_placement(sys, user, value)? => return Ok(None),
                Some(_) => {}
            }
        }
        None => {}
        Some((_, PValue::Array(_))) => {}
        _ => return Err("widget instances are not an array; left untouched".into()),
    }
    let mut next: Vec<_> = fields
        .iter()
        .filter(|(k, _)| k != "instances" && k != "DesktopWidgetPlacementStorage")
        .cloned()
        .collect();
    next.push(("instances".into(), PValue::Array(Vec::new())));
    Ok(Some(PValue::Dict(next)))
}

fn widget_placement_conversion(user: &User) -> Cmd {
    Cmd::new("/usr/bin/plutil")
        .args(["-convert", "xml1", "-o", "-", "--", "-"])
        .as_user(user)
}

/// NotificationCenter recreates this binary plist even with no widgets. Its empty
/// display list is metadata, not a configured widget; removing it on every probe
/// would cause an endless write/relaunch cycle. Unknown structures are cleared.
fn empty_widget_placement(sys: &dyn System, user: &User, value: &PValue) -> Result<bool, String> {
    let PValue::Data(bytes) = value else {
        return Ok(false);
    };
    let ran = sys
        .run_complete_stdout_input(&widget_placement_conversion(user), bytes)
        .map_err(|_| "cannot completely inspect desktop widget placement".to_owned())?;
    if !ran.success() {
        return Ok(false);
    }
    let PValue::Dict(fields) = crate::plist::parse(&ran.stdout)
        .map_err(|_| "unrecognized desktop widget placement property list".to_owned())?
    else {
        return Ok(false);
    };
    Ok(fields.len() == 2
        && fields
            .iter()
            .any(|(key, value)| key == "CompatibilityVersion" && *value == PValue::Int(1))
        && fields.iter().any(|(key, value)| {
            key == "NumberedDisplays"
                && matches!(value, PValue::Array(displays) if displays.is_empty())
        }))
}

fn dock_apps() -> PValue {
    PValue::Array(
        DOCK_APPS
            .into_iter()
            .map(|(path, bundle, label)| {
                PValue::Dict(vec![
                    ("tile-type".into(), PValue::Str("file-tile".into())),
                    (
                        "tile-data".into(),
                        PValue::Dict(vec![
                            ("bundle-identifier".into(), PValue::Str(bundle.into())),
                            ("file-label".into(), PValue::Str(label.into())),
                            (
                                "file-data".into(),
                                PValue::Dict(vec![
                                    (
                                        "_CFURLString".into(),
                                        PValue::Str(format!(
                                            "file://{}/",
                                            path.replace(' ', "%20")
                                        )),
                                    ),
                                    ("_CFURLStringType".into(), PValue::Int(15)),
                                ]),
                            ),
                        ]),
                    ),
                ])
            })
            .collect(),
    )
}

/// An asserted bundle identifier in Dock preferences does not establish an installed
/// application. Read the requested bundles before proposing pins, including through
/// Safari's system-managed symlink, so missing apps never produce broken success.
fn dock_apps_available(sys: &dyn System, user: &User) -> Result<(), String> {
    for (path, bundle, label) in DOCK_APPS {
        let contents = Path::new(path).join("Contents");
        if !sys.stat(&contents).is_ok_and(|s| s.dir) {
            return Err(format!(
                "{label} is not installed at {path}; Dock left untouched"
            ));
        }
        let cmd = Cmd::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
            .arg(contents.join("Info.plist"))
            .as_user(user);
        let ran = sys
            .run_complete_stdout(&cmd)
            .map_err(|_| format!("cannot verify {label} at {path}; Dock left untouched"))?;
        if !ran.success() || ran.out().trim() != bundle {
            return Err(format!(
                "{path} is not the requested {label} application; Dock left untouched"
            ));
        }
    }
    Ok(())
}

/// Dock adds bookmarks and GUIDs and changes URL escaping or trailing separators.
/// Compare the actual local file target as well as identity, type and order; a stale
/// bundle-identifier field on a missing or different target is not the requested pin.
fn same_apps(sys: &dyn System, now: &PValue, want: &PValue) -> bool {
    let (PValue::Array(now), PValue::Array(want)) = (now, want) else {
        return false;
    };
    now.len() == want.len()
        && now.iter().zip(want).all(|(now, want)| {
            field(now, "tile-type") == field(want, "tile-type")
                && field(now, "tile-data").and_then(|v| field(v, "bundle-identifier"))
                    == field(want, "tile-data").and_then(|v| field(v, "bundle-identifier"))
                && dock_app_path(now)
                    .zip(dock_app_path(want))
                    .is_some_and(|(now, want)| {
                        sys.canonicalize(&now).unwrap_or(now)
                            == sys.canonicalize(&want).unwrap_or(want)
                    })
        })
}

fn dock_app_path(pin: &PValue) -> Option<PathBuf> {
    let file = field(field(pin, "tile-data")?, "file-data")?;
    let PValue::Str(value) = field(file, "_CFURLString")? else {
        return None;
    };
    let path = match field(file, "_CFURLStringType")? {
        PValue::Int(0) => PathBuf::from(value),
        PValue::Int(15) => {
            let url = value.strip_prefix("file://")?;
            let (authority, encoded) = url.split_once('/')?;
            if !matches!(authority, "" | "localhost") || encoded.contains(['?', '#']) {
                return None;
            }
            let mut decoded = vec![b'/'];
            let mut bytes = encoded.as_bytes().iter().copied();
            while let Some(byte) = bytes.next() {
                decoded.push(if byte == b'%' {
                    let high = char::from(bytes.next()?).to_digit(16)?;
                    let low = char::from(bytes.next()?).to_digit(16)?;
                    u8::try_from(high * 16 + low).ok()?
                } else {
                    byte
                });
            }
            PathBuf::from(String::from_utf8(decoded).ok()?)
        }
        _ => return None,
    };
    (path.is_absolute()
        && !path.components().any(|c| c == Component::ParentDir)
        && !path.as_os_str().as_encoded_bytes().contains(&0))
    .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::Workstation;
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    /// A desktop item of its own pass.
    fn desktop(who: &User, kind: DesktopKind) -> Desktop {
        Desktop::new(who, kind, &Arc::new(DesktopActivation::new(who)))
    }

    fn installed_dock_apps(sys: &Fake, who: &User) {
        for (path, bundle, _) in DOCK_APPS {
            sys.stat_of(
                &format!("{path}/Contents"),
                Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o755,
                    dir: true,
                    symlink: false,
                    len: 0,
                },
            );
            let cmd = Cmd::new("/usr/bin/plutil")
                .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
                .arg(format!("{path}/Contents/Info.plist"))
                .as_user(who);
            sys.on(&cmd.display(), Ran::ok(bundle));
        }
    }

    #[test]
    fn hidutil_table_preserves_every_unrelated_mapping_and_updates_conflicting_caps() {
        let table = "RegistryID Key Value\n100000001 UserKeyMapping (\n{ HIDKeyboardModifierMappingDst = 30064771297; HIDKeyboardModifierMappingSrc = 30064771303; },\n{ HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771241; }\n)\n100000002 UserKeyMapping (null)\n";
        let maps = parse_hid_mappings(table).unwrap();
        assert_eq!(maps.len(), 2);
        assert!(
            merged_mapping(&maps).is_err(),
            "different services must never overwrite one another"
        );
        let want = merged_mapping(&maps[..1]).unwrap().unwrap();
        assert_eq!(
            want,
            [
                (30064771303, 30064771297),
                (keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL)
            ]
        );
        assert_eq!(merged_mapping(&[want]).unwrap(), None);
        assert!(parse_hid_mappings("100000001 UserKeyMapping (garbage)").is_err());
    }

    #[test]
    fn immediate_mapping_sets_only_the_matched_keyboard_and_verifies_the_real_hid_output() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
        };
        let sys = Fake::root();
        let get = item.command().args(["--get", "UserKeyMapping"]).display();
        sys.on(&get, Ran::ok("RegistryID Key Value\n10002160e UserKeyMapping (null)\n100021604 UserKeyMapping (null)\n"));
        let set = item.command().args(["--set", "{\"UserKeyMapping\":[{\"HIDKeyboardModifierMappingDst\":30064771296,\"HIDKeyboardModifierMappingSrc\":30064771129}]}"]).display();
        sys.on(&set, Ran::ok(""));
        let c = ctx(&[who]);
        assert!(matches!(item.probe(&sys, &c), Probe::Change { .. }));
        item.apply(&sys, &c).unwrap();
        assert!(sys.calls().contains(&set));
        sys.on(&get, Ran::ok("RegistryID Key Value\n10002160e UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n100021604 UserKeyMapping ( { HIDKeyboardModifierMappingDst = 30064771296; HIDKeyboardModifierMappingSrc = 30064771129; } )\n"));
        // Fake consumes the first existing response before advancing to the new one.
        let _ = item.mappings(&sys).unwrap();
        assert!(matches!(item.probe(&sys, &c), Probe::Ok { .. }));
    }

    #[test]
    fn an_incomplete_hid_record_never_overwrites_an_omitted_services_remaps() {
        let who = user("alice", 501);
        let item = AttachedKeyboard {
            model: keyboard::Model {
                vendor: 1452,
                product: 598,
            },
            user: who.clone(),
        };
        let sys = Fake::root();
        let get = item.command().args(["--get", "UserKeyMapping"]).display();
        // This retained tail would parse and suggest overwriting every service if
        // the read were allowed to discard earlier services' bytes.
        sys.on(
            &get,
            Ran::ok("RegistryID Key Value\n100021604 UserKeyMapping (null)\n"),
        );
        sys.refuse_complete_stdout(&get, "stdout exceeds complete capture bound");
        let rows = crate::engine::run(
            &[Box::new(item)],
            crate::model::Mode::Apply,
            &sys,
            &ctx(&[who]),
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, crate::model::Outcome::Unknown);
        assert!(sys.calls().iter().all(|call| !call.contains("--set")));
    }

    #[test]
    fn widget_clear_removes_instances_and_placement_but_keeps_unknown_widget_fields() {
        let sys = Fake::root();
        let who = user("alice", 501);
        sys.on(
            &widget_placement_conversion(&who).display(),
            Ran::exit(1, "invalid plist"),
        );
        let read = Read::Value(PValue::Dict(vec![
            (
                "instances".into(),
                PValue::Array(vec![PValue::Data(vec![1, 2])]),
            ),
            (
                "DesktopWidgetPlacementStorage".into(),
                PValue::Data(vec![3]),
            ),
            ("vers".into(), PValue::Int(1)),
            ("future".into(), PValue::Bool(true)),
        ]));
        let want = cleared_widgets(&sys, &who, &read).unwrap().unwrap();
        assert_eq!(field(&want, "instances"), Some(&PValue::Array(vec![])));
        assert_eq!(field(&want, "future"), Some(&PValue::Bool(true)));
        assert_eq!(field(&want, "DesktopWidgetPlacementStorage"), None);
        assert_eq!(
            cleared_widgets(&sys, &who, &Read::Value(want)).unwrap(),
            None
        );
        assert!(
            cleared_widgets(&sys, &who, &Read::Absent)
                .unwrap()
                .is_some()
        );
        let residual = Read::Value(PValue::Dict(vec![
            ("instances".into(), PValue::Array(Vec::new())),
            (
                "DesktopWidgetPlacementStorage".into(),
                PValue::Data(vec![3]),
            ),
            ("future".into(), PValue::Bool(true)),
        ]));
        let cleared = cleared_widgets(&sys, &who, &residual).unwrap().unwrap();
        assert_eq!(field(&cleared, "DesktopWidgetPlacementStorage"), None);
        assert_eq!(field(&cleared, "future"), Some(&PValue::Bool(true)));
        assert_eq!(
            cleared_widgets(&sys, &who, &Read::Value(cleared)).unwrap(),
            None
        );
    }

    #[test]
    fn notificationcenters_regenerated_empty_binary_placement_is_already_clear() {
        // Captured read-only on macOS 27.0.1 after NotificationCenter relaunched
        // with instances empty; CompatibilityVersion1 + NumberedDisplays[].
        let bytes = aterm_codec::base64::decode("YnBsaXN0MDDSAQIDBF8QFENvbXBhdGliaWxpdHlWZXJzaW9uXxAQTnVtYmVyZWREaXNwbGF5cxQAAAAAAAAAAAAAAAAAAAABoAgNJDdIAAAAAAAAAQEAAAAAAAAABQAAAAAAAAAAAAAAAAAAAEk=").unwrap();
        let sys = Fake::root();
        let who = user("alice", 501);
        let cmd = widget_placement_conversion(&who);
        let empty = PValue::Dict(vec![
            ("CompatibilityVersion".into(), PValue::Int(1)),
            ("NumberedDisplays".into(), PValue::Array(Vec::new())),
        ]);
        sys.on(&cmd.display(), export(&crate::plist::fragment(&empty)));
        let read = Read::Value(PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            ("instances".into(), PValue::Array(Vec::new())),
            ("DesktopWidgetPlacementStorage".into(), PValue::Data(bytes)),
        ]));
        assert_eq!(cleared_widgets(&sys, &who, &read).unwrap(), None);
        assert_eq!(sys.calls(), [cmd.display()]);
        // Only the known empty shape is accepted; stale displays or unknown
        // fields cannot be mistaken for an empty desktop layout.
        for shape in [
            PValue::Dict(vec![
                ("CompatibilityVersion".into(), PValue::Int(1)),
                (
                    "NumberedDisplays".into(),
                    PValue::Array(vec![PValue::Int(1)]),
                ),
            ]),
            PValue::Dict(vec![
                ("CompatibilityVersion".into(), PValue::Int(1)),
                ("NumberedDisplays".into(), PValue::Array(Vec::new())),
                ("future".into(), PValue::Bool(true)),
            ]),
        ] {
            let next = Fake::root();
            next.on(&cmd.display(), export(&crate::plist::fragment(&shape)));
            assert!(cleared_widgets(&next, &who, &read).unwrap().is_some());
        }
        sys.refuse_complete_stdout(&cmd.display(), "incomplete XML capture");
        assert!(cleared_widgets(&sys, &who, &read).is_err());
    }

    #[test]
    fn dock_metadata_is_ignored_but_pin_order_extra_pins_and_stacks_are_not() {
        let want = dock_apps();
        let PValue::Array(mut pins) = want.clone() else {
            unreachable!()
        };
        let PValue::Dict(first) = &mut pins[0] else {
            unreachable!()
        };
        first.push(("GUID".into(), PValue::Int(123)));
        assert!(same_apps(
            &Fake::root(),
            &PValue::Array(pins.clone()),
            &want
        ));
        pins.swap(0, 1);
        assert!(!same_apps(
            &Fake::root(),
            &PValue::Array(pins.clone()),
            &want
        ));
        pins.push(PValue::Dict(vec![]));
        assert!(!same_apps(&Fake::root(), &PValue::Array(pins), &want));
    }

    #[test]
    fn dock_bundle_labels_cannot_hide_missing_or_different_file_targets() {
        let want = dock_apps();
        let xml = crate::plist::fragment(&want);
        for wrong in [
            "file:///Applications/Missing.app/",
            "file://another-host/Applications/aterm.app/",
            "file:///Applications/aterm.app/?different-target",
            "file:///Applications/aterm.app/#different-target",
            "file:///Applications/%QQaterm.app/",
            "file:///Applications/%00aterm.app/",
        ] {
            let changed = xml.replace("file:///Applications/aterm.app/", wrong);
            let changed = crate::plist::parse(changed.as_bytes()).unwrap();
            assert!(!same_apps(&Fake::root(), &changed, &want), "{wrong}");
        }
        let missing = xml.replacen("<key>file-data</key>", "<key>unusable-file-data</key>", 1);
        assert!(!same_apps(
            &Fake::root(),
            &crate::plist::parse(missing.as_bytes()).unwrap(),
            &want
        ));
        let normalized = xml
            .replace(
                "file:///Applications/aterm.app/",
                "file://localhost/Applications/%61term.app",
            )
            .replace(
                "file:///Applications/Safari.app/",
                "file:///Applications/Safari.app",
            );
        assert!(same_apps(
            &Fake::root(),
            &crate::plist::parse(normalized.as_bytes()).unwrap(),
            &want
        ));
    }

    #[test]
    fn missing_or_wrong_installed_dock_apps_cannot_be_reported_configured() {
        let who = user("alice", 501);
        for wrong_identity in [false, true] {
            let sys = Fake::root();
            if wrong_identity {
                let cmd = Cmd::new("/usr/bin/plutil")
                    .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
                    .arg("/Applications/aterm.app/Contents/Info.plist")
                    .as_user(&who);
                sys.on(&cmd.display(), Ran::ok("com.example.another-app"));
                installed_dock_apps(&sys, &who);
            }
            let item = desktop(&who, DesktopKind::Dock);
            let rows = crate::engine::run(
                &[Box::new(item)],
                crate::model::Mode::Apply,
                &sys,
                &ctx(std::slice::from_ref(&who)),
                &mut crate::engine::NoJournal,
            );
            assert_eq!(rows[0].outcome, crate::model::Outcome::Unknown);
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" write ") && !call.contains("killall"))
            );
        }
    }

    #[test]
    fn dock_plan_is_read_only_and_covers_every_persistent_static_and_recent_section() {
        let who = user("alice", 501);
        let item = desktop(&who, DesktopKind::Dock);
        let sys = Fake::root();
        installed_dock_apps(&sys, &who);
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.dock -",
            export("<dict/>"),
        );
        let changes = item.changes(&sys).unwrap();
        let keys: Vec<_> = changes
            .iter()
            .map(|(target, _, _)| target.key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "persistent-apps",
                "persistent-others",
                "static-apps",
                "static-others",
                "recent-apps",
                "show-recents",
                "static-only",
                "minimize-to-application"
            ]
        );
        assert!(sys.calls().iter().all(|call| call.contains(" export ")
            || call.contains("/usr/bin/plutil -extract CFBundleIdentifier")));
        let mut c = ctx(&[who]);
        c.disrupt = false;
        assert!(matches!(item.probe(&sys, &c), Probe::Deferred { .. }));
        let undo = item.revert().unwrap();
        assert_eq!(undo.lines().count(), keys.len());
        assert!(
            undo.lines()
                .all(|line| line.contains("/usr/bin/defaults delete") && line.contains("  # "))
        );
    }

    #[test]
    fn a_failed_namespace_flush_allows_no_write_and_a_retry_flushes_the_whole_chain() {
        let who = user("alice", 501);
        let windowmanager = "alice$ /usr/bin/defaults export com.apple.WindowManager -";
        let notificationcenter = "alice$ /usr/bin/defaults export com.apple.notificationcenterui -";
        let c = ctx(std::slice::from_ref(&who));
        // A failed namespace flush cannot allow any preference write or activation.
        // Retry must flush the whole chain even though its directories now exist.
        let directories = [
            "/Users//alice/Library/Application Support/aterm/init-activation",
            "/Users//alice/Library/Application Support/aterm",
            "/Users//alice/Library/Application Support",
            "/Users//alice/Library",
            "/Users//alice",
            "/Users",
            "/",
        ];
        for (failed_at, directory) in directories.iter().enumerate() {
            let mut sys = Fake::root();
            sys.euid = who.uid;
            sys.on(windowmanager, export("<dict><key>StandardHideWidgets</key><false/><key>StageManagerHideWidgets</key><false/></dict>"));
            sys.on(
                notificationcenter,
                export("<dict><key>widgets</key><dict><key>instances</key><array/></dict></dict>"),
            );
            for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
                let cmd = Target::user("com.apple.WindowManager", key, &who)
                    .write_cmd(&PValue::Bool(true));
                sys.on(&cmd.display(), Ran::ok(""));
            }
            sys.durable_dir_fails(Path::new(directory), Some(std::io::ErrorKind::Other));
            let item = desktop(&who, DesktopKind::Widgets);
            assert!(item.apply(&sys, &c).is_err());
            assert!(crate::init_activation::pending(&sys, &who, "widgets").unwrap());
            let calls = sys.calls();
            assert!(
                calls
                    .iter()
                    .all(|call| !call.contains("/usr/bin/defaults write")
                        && !call.contains("pgrep")
                        && !call.contains("killall"))
            );
            let flushes: Vec<_> = calls
                .iter()
                .filter(|call| call.starts_with("durable-dir "))
                .cloned()
                .collect();
            let expected: Vec<_> = directories
                .iter()
                .map(|dir| format!("durable-dir {dir}"))
                .collect();
            assert_eq!(flushes, expected[..=failed_at]);
            sys.durable_dir_fails(Path::new(directory), None);
            let retry = calls.len();
            item.apply(&sys, &c).unwrap();
            // Written, and its relaunch owed to the pass's activation, which runs after it.
            assert!(crate::init_activation::pending(&sys, &who, "widgets").unwrap());
            let calls = sys.calls();
            let retry_calls = &calls[retry..];
            let first_write = retry_calls
                .iter()
                .position(|call| call.contains("/usr/bin/defaults write"))
                .unwrap();
            let flushed_before_write: Vec<_> = retry_calls[..first_write]
                .iter()
                .filter(|call| call.starts_with("durable-dir "))
                .cloned()
                .collect();
            assert_eq!(flushed_before_write, expected);
            assert_eq!(
                retry_calls
                    .iter()
                    .filter(|call| call.contains("/usr/bin/defaults write"))
                    .count(),
                2
            );
        }
    }

    /// The tiles a Dock reads as it starts and writes back.
    const TILES: [&str; 3] = ["persistent-apps", "persistent-others", "recent-apps"];

    /// The tiles a starting Dock read, each by its key.
    type Loaded = Vec<(&'static str, PValue)>;

    /// A starting process's tiles and the sleeps left until its start is past.
    type Starting = Option<(Loaded, u32)>;

    /// A desktop the [`Fake`] runs ([`Fake::simulate`]): `defaults` reads and writes one
    /// store, and `pgrep`, `killall` and `sleep` act on its Dock, Finder and Notification
    /// Center, each started again at once when it is killed, as launchd does. A Dock reads
    /// its tiles ([`TILES`]) as it starts and is past its start `start` sleeps later, when
    /// it writes them back with the GUIDs it adds (a managed one, the pins it is made to
    /// show). Killed before that, it writes what it read back as it exits: the pins of a
    /// Dock relaunched between two writes it reads come back over the second, as on the
    /// owner's Mac on 2026-10-21.
    struct Desk {
        prefs: Vec<(String, String, PValue)>,
        /// Each running process, its pid, and, while it is starting, the tiles it read and
        /// the sleeps left until its start is past.
        running: Vec<(&'static str, u32, Starting)>,
        next_pid: u32,
        /// How many sleeps a Dock takes to get past its start.
        start: u32,
        /// The pins a managed Dock shows once it is past its start, whatever was written.
        managed: Option<PValue>,
        /// A process launchd does not start again once it is killed.
        stays_down: Option<&'static str>,
        /// A process whose `killall` fails (exit 2) and signals nothing.
        unkillable: Option<&'static str>,
        /// A `defaults write` of this domain and key fails, writing nothing.
        unwritable: Option<(&'static str, &'static str)>,
        /// How many `launchctl print` session checks fail before one answers.
        session_failures: u32,
        /// What happened, in order: `write <domain> <key>`, `kill <process>`, `<process>
        /// <pid> wrote back what it read at its start`, `Dock <pid> past its start`.
        events: Vec<String>,
    }

    fn pin(label: &str) -> PValue {
        PValue::Dict(vec![(
            "tile-data".into(),
            PValue::Dict(vec![("file-label".into(), PValue::Str(label.into()))]),
        )])
    }

    impl Desk {
        /// A desktop before setup: other pins, a stack, a recent app, a widget, hidden
        /// desktop icons; the Dock, Finder and Notification Center running, past their start.
        fn before() -> Self {
            Self {
                prefs: vec![
                    (
                        DOCK.into(),
                        "persistent-apps".into(),
                        PValue::Array(vec![pin("Mail"), pin("Music")]),
                    ),
                    (
                        DOCK.into(),
                        "persistent-others".into(),
                        PValue::Array(vec![pin("Downloads")]),
                    ),
                    (
                        DOCK.into(),
                        "recent-apps".into(),
                        PValue::Array(vec![pin("Notes")]),
                    ),
                    (DOCK.into(), "show-recents".into(), PValue::Bool(true)),
                    (
                        WINDOW_MANAGER.into(),
                        "StandardHideDesktopIcons".into(),
                        PValue::Bool(true),
                    ),
                    (
                        NOTIFICATION_CENTER.into(),
                        "widgets".into(),
                        PValue::Dict(vec![
                            ("vers".into(), PValue::Int(1)),
                            (
                                "instances".into(),
                                PValue::Array(vec![PValue::Data(vec![42])]),
                            ),
                        ]),
                    ),
                ],
                running: vec![
                    ("Dock", 100, None),
                    ("Finder", 200, None),
                    ("NotificationCenter", 300, None),
                ],
                next_pid: 1000,
                start: 1,
                managed: None,
                stays_down: None,
                unkillable: None,
                unwritable: None,
                session_failures: 0,
                events: Vec::new(),
            }
        }

        fn read(&self, domain: &str, key: &str) -> Option<&PValue> {
            self.prefs
                .iter()
                .find(|(d, k, _)| d == domain && k == key)
                .map(|(_, _, value)| value)
        }

        fn write(&mut self, domain: &str, key: &str, value: PValue) {
            self.prefs.retain(|(d, k, _)| !(d == domain && k == key));
            self.prefs.push((domain.into(), key.into(), value));
        }

        fn count(&self, event: &str) -> usize {
            self.events.iter().filter(|e| *e == event).count()
        }

        /// Each process still starting, by name and pid.
        fn starting(&self) -> Vec<(&'static str, u32)> {
            self.running
                .iter()
                .filter(|(_, _, starting)| starting.is_some())
                .map(|(name, pid, _)| (*name, *pid))
                .collect()
        }

        fn answer(&mut self, fake: &Fake, cmd: &Cmd) -> Option<Ran> {
            let args: Vec<String> = cmd
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match (cmd.program.to_str()?, args.as_slice()) {
                ("/usr/bin/defaults", ["export", domain, "-"]) => {
                    let entries = self
                        .prefs
                        .iter()
                        .filter(|(d, _, _)| d == domain)
                        .map(|(_, key, value)| (key.clone(), value.clone()))
                        .collect();
                    Some(Ran::ok(&crate::plist::document(&PValue::Dict(entries))))
                }
                ("/usr/bin/defaults", ["write", domain, key, value @ ..]) => {
                    let value = match value {
                        ["-bool", b] => PValue::Bool(*b == "true"),
                        ["-int", i] => PValue::Int(i.parse().ok()?),
                        ["-string", text] => PValue::Str((*text).into()),
                        [fragment] => crate::plist::parse(fragment.as_bytes()).ok()?,
                        _ => return None,
                    };
                    if self.unwritable == Some((domain, key)) {
                        return Some(Ran::exit(1, "injected write failure"));
                    }
                    self.events.push(format!("write {domain} {key}"));
                    self.write(domain, key, value);
                    Some(Ran::ok(""))
                }
                ("/usr/bin/pgrep", ["-u", _, "-x", process]) => {
                    let pids: String = self
                        .running
                        .iter()
                        .filter(|(name, _, _)| name == process)
                        .map(|(_, pid, _)| format!("{pid}\n"))
                        .collect();
                    Some(if pids.is_empty() {
                        Ran {
                            status: Some(1),
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        }
                    } else {
                        Ran::ok(&pids)
                    })
                }
                ("/bin/launchctl", ["print", _]) => Some(if self.session_failures > 0 {
                    self.session_failures -= 1;
                    Ran::exit(2, "injected session lookup failure")
                } else {
                    Ran::ok("")
                }),
                ("/usr/bin/killall", ["-u", _, process]) => {
                    if self.unkillable == Some(*process) {
                        return Some(Ran::exit(2, "injected killall failure"));
                    }
                    self.kill(fake, process);
                    Some(Ran::ok(""))
                }
                ("/bin/sleep", ["1"]) => {
                    self.tick();
                    Some(Ran::ok(""))
                }
                _ => None,
            }
        }

        fn kill(&mut self, fake: &Fake, process: &str) {
            let dying: Vec<_> = self
                .running
                .iter()
                .filter(|(name, _, _)| *name == process)
                .cloned()
                .collect();
            self.running.retain(|(name, _, _)| *name != process);
            for (name, pid, starting) in dying {
                if let Some((tiles, _)) = starting {
                    for (key, value) in tiles {
                        self.write(DOCK, key, value);
                    }
                    self.events
                        .push(format!("{name} {pid} wrote back what it read at its start"));
                }
                fake.exited(pid);
                self.events.push(format!("kill {name}"));
                if self.stays_down == Some(name) {
                    continue;
                }
                let pid = self.next_pid;
                self.next_pid += 1;
                let starting = (name == "Dock").then(|| {
                    let tiles = TILES
                        .iter()
                        .filter_map(|key| self.read(DOCK, key).map(|value| (*key, value.clone())))
                        .collect();
                    (tiles, self.start)
                });
                self.running.push((name, pid, starting));
                fake.alive(pid);
            }
        }

        /// A second passes: each starting Dock is a second further, and one past its start
        /// writes its tiles back, each with a GUID.
        fn tick(&mut self) {
            let mut started = Vec::new();
            for (name, pid, starting) in &mut self.running {
                if let Some((_, left)) = starting {
                    *left = left.saturating_sub(1);
                    if *left == 0
                        && let Some((tiles, _)) = starting.take()
                    {
                        started.push((*name, *pid, tiles));
                    }
                }
            }
            for (name, pid, tiles) in started {
                for (key, value) in tiles {
                    let value = match (&self.managed, key) {
                        (Some(managed), "persistent-apps") => managed.clone(),
                        _ => value,
                    };
                    self.write(DOCK, key, with_guids(value, i64::from(pid)));
                }
                self.events.push(format!("{name} {pid} past its start"));
            }
        }
    }

    /// `tiles` as a Dock rewrites them: each pin with a GUID.
    fn with_guids(tiles: PValue, guid: i64) -> PValue {
        let PValue::Array(pins) = tiles else {
            return tiles;
        };
        PValue::Array(
            pins.into_iter()
                .map(|pin| match pin {
                    PValue::Dict(mut fields) => {
                        if !fields.iter().any(|(key, _)| key == "GUID") {
                            fields.push(("GUID".into(), PValue::Int(guid)));
                        }
                        PValue::Dict(fields)
                    }
                    other => other,
                })
                .collect(),
        )
    }

    /// `desk`, run by a fake acting as `who`, with the Dock's three apps installed.
    fn desktop_of(who: &User, desk: Desk) -> (Fake, Arc<Mutex<Desk>>) {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        installed_dock_apps(&sys, who);
        for (_, pid, _) in &desk.running {
            sys.alive(*pid);
        }
        let desk = Arc::new(Mutex::new(desk));
        let model = Arc::clone(&desk);
        sys.simulate(move |fake, cmd| model.lock().unwrap().answer(fake, cmd));
        (sys, desk)
    }

    /// The widget, desktop and Dock items of one pass, as setup selects them.
    fn desktop_items(who: &User) -> Vec<Box<dyn Item>> {
        selected_desktop_items(who, &[Setting::Widgets, Setting::Desktop, Setting::Dock])
    }

    /// The items of `settings` of one pass, as setup selects them.
    fn selected_desktop_items(who: &User, settings: &[Setting]) -> Vec<Box<dyn Item>> {
        let mut selected = Selection::none();
        for &setting in settings {
            selected.set(setting, true);
        }
        selected_items(&ctx(std::slice::from_ref(who)), selected).unwrap()
    }

    /// The pass's journal lines, each as its id and `to`.
    fn journaled(sys: &Fake) -> Vec<(String, String)> {
        String::from_utf8(sys.written(Fake::JOURNAL).unwrap())
            .unwrap()
            .lines()
            .map(|line| {
                let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                (
                    line["id"].as_str().unwrap().to_owned(),
                    line["to"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn verified(pass: &crate::init::WorkstationRun) -> Vec<(&str, bool)> {
        pass.attempts
            .iter()
            .map(|attempt| (attempt.id.as_str(), attempt.verified))
            .collect()
    }

    /// One pass of `items`, as doctor and init run them (through the snapshot guard).
    fn pass(sys: &Fake, items: Vec<Box<dyn Item>>, mode: Mode) -> crate::init::WorkstationRun {
        Workstation::of(items).run_recording(sys, mode).unwrap()
    }

    fn outcomes(pass: &crate::init::WorkstationRun) -> Vec<(&str, Outcome)> {
        pass.rows
            .iter()
            .map(|row| (row.id.as_str(), row.outcome))
            .collect()
    }

    const DESKTOP_IDS: [&str; 3] = [
        "init.widgets@alice",
        "init.desktop@alice",
        "init.dock@alice",
    ];

    /// The owner's Mac, 2026-10-21 23:01 to 02:24: each unattended pass wrote the desktop
    /// keys and relaunched the Dock and Finder, wrote the Dock's pins a second later and
    /// relaunched the Dock again. The Dock it killed was still starting and wrote back the
    /// pins it had read, so pass after pass read "applied, but it still reads
    /// com.apple.dock persistent-apps, com.apple.dock persistent-others, com.apple.dock
    /// recent-apps". Now every desktop write of the pass lands first; the Dock, Finder and
    /// Notification Center relaunch once each, journaled after the items' own lines; and
    /// each item is read back once those are up and past their start. Control: the same
    /// desktop driven in the old order gets its old pins back. Negative control: the code
    /// before this change fails this test with exactly the owner's read-back.
    #[test]
    fn one_pass_writes_every_desktop_key_before_it_relaunches_each_reader_once() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed)),
            "{:?}",
            run.rows
        );
        assert!(run.attempts.iter().all(|attempt| attempt.verified));
        let desk = desk.lock().unwrap();
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1),
            "{:?}",
            desk.events
        );
        let first_kill = desk
            .events
            .iter()
            .position(|e| e.starts_with("kill "))
            .unwrap();
        let last_write = desk
            .events
            .iter()
            .rposition(|e| e.starts_with("write "))
            .unwrap();
        assert!(last_write < first_kill, "{:?}", desk.events);
        // The read-back waited for the one new Dock to be past its start.
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("Dock ") && e.ends_with(" past its start"))
                .count(),
            1,
            "{:#?}",
            desk.events
        );
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps()
        ));
        let ids: Vec<String> = journaled(&sys).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            [
                "init.widgets@alice",
                "init.desktop@alice",
                "init.dock@alice",
                "init.relaunch@alice"
            ]
        );
        for family in ["widgets", "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        drop(desk);

        // Control: the old order, the desktop's relaunch a moment before the Dock's write.
        let (sys, desk) = desktop_of(&who, Desk::before());
        let pins = Target::user(DOCK, "persistent-apps", &who);
        defaults::relaunch(&sys, &who, "Dock").unwrap();
        sys.run(&pins.write_cmd(&dock_apps())).unwrap();
        defaults::relaunch(&sys, &who, "Dock").unwrap();
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.read(DOCK, "persistent-apps"),
            Some(&PValue::Array(vec![pin("Mail"), pin("Music")])),
            "{:?}",
            desk.events
        );
    }

    /// A read-back is what the relaunched reader settled on, not what was written: a
    /// managed Dock that shows its own pins once it is past its start reads back as not
    /// changed (and is put back by the doctor), while the items whose readers kept what was
    /// written read back changed. Negative control: `relaunch_settled` returning once the old
    /// processes exited reads the Dock back before its start is past, and calls it changed.
    /// A Dock slower to start than a second apart is waited on until it has loaded the
    /// tiles it was handed (each gets a GUID): two equal reads of a Dock that has not yet
    /// rewritten them say nothing. Negative control: a look that ignores the tiles' GUIDs
    /// reads the slow managed Dock back changed.
    #[test]
    fn the_read_back_waits_until_the_relaunched_dock_is_past_its_start() {
        let who = user("alice", 501);
        let mut managed = Desk::before();
        managed.managed = Some(PValue::Array(vec![pin("Mail")]));
        let (sys, _desk) = desktop_of(&who, managed);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                ("init.widgets@alice", Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ]
        );
        assert_eq!(
            run.rows[2].why.as_deref(),
            Some("applied, but it still reads com.apple.dock persistent-apps")
        );
        assert_eq!(
            verified(&run),
            [
                ("init.widgets@alice", true),
                ("init.desktop@alice", true),
                ("init.dock@alice", false)
            ]
        );
        // The same Dock, three seconds past its relaunch before it shows its pins.
        let mut slow = Desk::before();
        slow.managed = Some(PValue::Array(vec![pin("Mail")]));
        slow.start = 3;
        let (sys, desk) = desktop_of(&who, slow);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                ("init.widgets@alice", Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:#?}",
            desk.lock().unwrap().events
        );
        assert!(desk.lock().unwrap().starting().is_empty());
    }

    /// A pass that ends after its writes and before the relaunch (the process quit, the
    /// Mac slept) leaves every family's marker. The next pass writes nothing again,
    /// relaunches each reader once, and clears them.
    #[test]
    fn a_pass_that_ends_between_its_writes_and_the_relaunch_relaunches_on_the_next() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let c = ctx(std::slice::from_ref(&who));
        // What the engine does before it reaches the activation; the process ends there.
        for item in desktop_items(&who) {
            item.apply(&sys, &c).unwrap();
        }
        for family in ["widgets", "desktop", "dock"] {
            assert!(crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        let writes = {
            let desk = desk.lock().unwrap();
            assert!(!desk.events.iter().any(|e| e.starts_with("kill ")));
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count()
        };
        let checked = pass(&sys, desktop_items(&who), Mode::Plan);
        assert_eq!(
            outcomes(&checked),
            DESKTOP_IDS.map(|id| (id, Outcome::Planned))
        );
        let next = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&next),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed))
        );
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count(),
            writes
        );
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter")
            ),
            (1, 1, 1)
        );
        for family in ["widgets", "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
    }

    /// A relaunch that cannot be made (the GUI session lookup for the Dock failed) fails
    /// every family the Dock reads, here all three, and keeps the written preferences and
    /// their markers, while the readers that could relaunch were (each once) and were
    /// waited on: a plan still plans them, and the next pass relaunches each reader once
    /// without writing again.
    #[test]
    fn a_relaunch_that_fails_keeps_the_writes_pending_for_the_next_pass() {
        let who = user("alice", 501);
        let mut failing = Desk::before();
        failing.session_failures = 1;
        let (sys, desk) = desktop_of(&who, failing);
        let first = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&first),
            DESKTOP_IDS.map(|id| (id, Outcome::Failed))
        );
        for row in &first.rows {
            let why = row.why.as_deref().unwrap();
            assert!(
                why.starts_with(
                    "applied, but not activated: written, but /bin/launchctl print gui/501"
                ),
                "{why}"
            );
        }
        for family in ["widgets", "desktop", "dock"] {
            assert!(crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        let (writes, kills) = {
            let desk = desk.lock().unwrap();
            let kills = (
                desk.count("kill Dock"),
                desk.count("kill Finder"),
                desk.count("kill NotificationCenter"),
            );
            assert_eq!(kills, (0, 1, 1), "{:?}", desk.events);
            assert_eq!(desk.starting(), []);
            let writes = desk
                .events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count();
            (writes, kills)
        };
        assert!(writes > 0);
        let checked = pass(&sys, desktop_items(&who), Mode::Plan);
        assert_eq!(
            outcomes(&checked),
            DESKTOP_IDS.map(|id| (id, Outcome::Planned))
        );
        let recovered = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&recovered),
            DESKTOP_IDS.map(|id| (id, Outcome::Changed))
        );
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.events
                .iter()
                .filter(|e| e.starts_with("write "))
                .count(),
            writes
        );
        assert_eq!(
            (
                desk.count("kill Dock") - kills.0,
                desk.count("kill Finder") - kills.1,
                desk.count("kill NotificationCenter") - kills.2
            ),
            (1, 1, 1)
        );
    }

    /// Each family settles alone: a reader that does not start again (Notification Center,
    /// here never started again by launchd) fails the families whose keys it reads and
    /// keeps their markers, while the families all of whose readers are up are read back,
    /// verified, and cleared; the doctor then rolls back only the widgets, and leaves the
    /// Dock's change that took. Negative control: failing every family on any reader's
    /// failure fails the desktop and Dock rows too.
    #[test]
    fn a_reader_that_does_not_start_again_fails_only_the_families_whose_keys_it_reads() {
        let who = user("alice", 501);
        let mut down = Desk::before();
        down.stays_down = Some("NotificationCenter");
        let (sys, desk) = desktop_of(&who, down);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                ("init.widgets@alice", Outcome::Failed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        assert_eq!(
            run.rows[0].why.as_deref(),
            Some(
                "applied, but not activated: NotificationCenter has not started again a \
                 minute after its relaunch; activation remains pending"
            )
        );
        assert_eq!(
            verified(&run),
            [
                ("init.widgets@alice", false),
                ("init.desktop@alice", true),
                ("init.dock@alice", true)
            ]
        );
        for (family, pending) in [("widgets", true), ("desktop", false), ("dock", false)] {
            assert_eq!(
                crate::init_activation::pending(&sys, &who, family).unwrap(),
                pending,
                "{family}"
            );
        }
        let desk = desk.lock().unwrap();
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps()
        ));
        assert_eq!(desk.count("kill Dock"), 1, "{:?}", desk.events);
    }

    /// A relaunch that cannot be made (Finder's `killall` fails, signalling nothing) fails
    /// the family it reads, and the wait for the readers already relaunched goes on: the
    /// Dock, slow to start, is past its start before the pass ends, so the doctor's
    /// rollback of the desktop keys and its relaunch of their readers kill a Dock that is
    /// up, which writes nothing back. Negative control: returning at the failed relaunch
    /// leaves the Dock starting.
    #[test]
    fn a_relaunch_that_cannot_be_made_still_waits_for_the_readers_already_relaunched() {
        let who = user("alice", 501);
        let mut stuck = Desk::before();
        stuck.unkillable = Some("Finder");
        stuck.start = 2;
        let (sys, desk) = desktop_of(&who, stuck);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                ("init.widgets@alice", Outcome::Changed),
                ("init.desktop@alice", Outcome::Failed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        let why = run.rows[1].why.as_deref().unwrap();
        assert!(
            why.starts_with("applied, but not activated: written, but ")
                && why.contains("killall -u alice Finder"),
            "{why}"
        );
        assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
        assert_eq!(desk.lock().unwrap().starting(), []);
        // The doctor puts back what did not verify, then relaunches what reads it.
        let icons = Target::user(WINDOW_MANAGER, "StandardHideDesktopIcons", &who);
        sys.run(&icons.write_cmd(&PValue::Bool(true))).unwrap();
        defaults::relaunch(&sys, &who, "Dock").unwrap();
        let desk = desk.lock().unwrap();
        assert_eq!(
            desk.read(WINDOW_MANAGER, "StandardHideDesktopIcons"),
            Some(&PValue::Bool(true))
        );
        assert!(
            !desk
                .events
                .iter()
                .any(|e| e.ends_with("wrote back what it read at its start")),
            "{:#?}",
            desk.events
        );
        assert!(same_apps(
            &sys,
            desk.read(DOCK, "persistent-apps").unwrap(),
            &dock_apps()
        ));
    }

    /// A family is owed its relaunch from its first write: one whose write fails part way
    /// has still made the writes before it, so its readers relaunch after them with the
    /// pass's other families, its marker is cleared, and the relaunch's line names it.
    /// The rest it did not write are written on the next pass. Negative control: owing the
    /// family only after its last write leaves its marker pending and its item unnamed.
    #[test]
    fn a_family_whose_write_fails_part_way_is_relaunched_for_what_it_wrote() {
        let who = user("alice", 501);
        let mut partial = Desk::before();
        partial.unwritable = Some((DOCK, "static-apps"));
        let (sys, desk) = desktop_of(&who, partial);
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run),
            [
                ("init.widgets@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:?}",
            run.rows
        );
        assert!(
            run.rows[1]
                .why
                .as_deref()
                .is_some_and(|why| why.contains("static-apps")),
            "{:?}",
            run.rows[1]
        );
        {
            let desk = desk.lock().unwrap();
            let wrote = desk
                .events
                .iter()
                .position(|e| e == "write com.apple.dock persistent-apps")
                .unwrap();
            let killed = desk.events.iter().position(|e| e == "kill Dock").unwrap();
            assert!(wrote < killed, "{:?}", desk.events);
        }
        assert!(!crate::init_activation::pending(&sys, &who, "dock").unwrap());
        let relaunch = journaled(&sys)
            .into_iter()
            .find(|(id, _)| id == "init.relaunch@alice")
            .unwrap();
        assert!(relaunch.1.contains("init.dock@alice"), "{relaunch:?}");
    }
}
