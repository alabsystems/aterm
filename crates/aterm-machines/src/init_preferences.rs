// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local `aterm init` desktop preferences. Persistent modifier mappings use the
//! same items as machine baselines; a separate HID item updates attached keyboards now.
//! No login agent or background process is installed.

use crate::items::defaults::{self, Target};
use crate::items::{Ctx, Item, keyboard};
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::port::{Cmd, System};
use aterm_types::macos_setup::{Selection, Setting};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const SRC: &str = "HIDKeyboardModifierMappingSrc";
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
    for (setting, kind) in [
        (Setting::Widgets, DesktopKind::Widgets),
        (Setting::Desktop, DesktopKind::Desktop),
        (Setting::Dock, DesktopKind::Dock),
    ] {
        if selected.contains(setting) {
            items.push(Box::new(Desktop {
                user: user.clone(),
                kind,
                before: Mutex::new(Vec::new()),
            }));
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

#[derive(Debug, Clone, Copy)]
enum DesktopKind {
    Widgets,
    Desktop,
    Dock,
}

#[derive(Debug)]
struct Desktop {
    user: User,
    kind: DesktopKind,
    before: Mutex<Vec<(Target, Read<PValue>)>>,
}

type Changes = Vec<(Target, Read<PValue>, PValue)>;

impl Desktop {
    fn target(&self, domain: &str, key: &str) -> Target {
        Target::user(domain, key, &self.user)
    }

    fn changes(&self, sys: &dyn System) -> Result<Changes, String> {
        let mut changes = Vec::new();
        let mut consider =
            |target: Target, want: PValue, semantic: &dyn Fn(&PValue, &PValue) -> bool| {
                let read = target.read(sys);
                match &read {
                    Read::Unusable(why) => return Err(why.clone()),
                    Read::Value(now) if semantic(now, &want) => return Ok(()),
                    Read::Absent | Read::Value(_) => {}
                }
                changes.push((target, read, want));
                Ok(())
            };
        match self.kind {
            DesktopKind::Widgets => {
                for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
                    consider(
                        self.target("com.apple.WindowManager", key),
                        PValue::Bool(true),
                        &equivalent,
                    )?;
                }
                let target = self.target("com.apple.notificationcenterui", "widgets");
                let read = target.read(sys);
                let want = cleared_widgets(sys, &self.user, &read)?;
                if let Some(want) = want {
                    changes.push((target, read, want));
                }
            }
            DesktopKind::Desktop => {
                for key in [
                    "EnableStandardClickToShowDesktop",
                    "StandardHideDesktopIcons",
                    "StageManagerHideDesktopIcons",
                ] {
                    consider(
                        self.target("com.apple.WindowManager", key),
                        PValue::Bool(false),
                        &equivalent,
                    )?;
                }
                consider(
                    self.target("com.apple.finder", "CreateDesktop"),
                    PValue::Bool(true),
                    &equivalent,
                )?;
            }
            DesktopKind::Dock => {
                dock_apps_available(sys, &self.user)?;
                consider(
                    self.target("com.apple.dock", "persistent-apps"),
                    dock_apps(),
                    &|now, want| same_apps(sys, now, want),
                )?;
                for key in [
                    "persistent-others",
                    "static-apps",
                    "static-others",
                    "recent-apps",
                ] {
                    consider(
                        self.target("com.apple.dock", key),
                        PValue::Array(Vec::new()),
                        &equivalent,
                    )?;
                }
                for (key, value) in [
                    ("show-recents", false),
                    ("static-only", false),
                    ("minimize-to-application", true),
                ] {
                    consider(
                        self.target("com.apple.dock", key),
                        PValue::Bool(value),
                        &equivalent,
                    )?;
                }
            }
        }
        Ok(changes)
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
        format!("init.{}@{}", self.family(), self.user.name)
    }

    fn family(&self) -> &'static str {
        match self.kind {
            DesktopKind::Widgets => "widgets",
            DesktopKind::Desktop => "desktop",
            DesktopKind::Dock => "dock",
        }
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

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let changes = self.changes(sys)?;
        let pending = crate::init_activation::pending(sys, &self.user, self.family())?;
        if changes.is_empty() && !pending {
            return Ok(());
        }
        crate::init_activation::begin(sys, &self.user, self.family())?;
        for (target, _, want) in changes {
            run(sys, &target.write_cmd(&want))?;
        }
        crate::init_activation::activate(sys, &self.user, self.family())?;
        crate::init_activation::finish(sys, &self.user, self.family())
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
    use crate::items::defaults::tests::{ctx, export, user};
    use crate::port::{Fake, Ran, Stat};

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
            let item = Desktop {
                user: who.clone(),
                kind: DesktopKind::Dock,
                before: Mutex::new(Vec::new()),
            };
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
        let item = Desktop {
            user: who.clone(),
            kind: DesktopKind::Dock,
            before: Mutex::new(Vec::new()),
        };
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
    fn written_preferences_do_not_hide_failed_activation_and_rerun_recovers_without_rewriting_them()
    {
        let who = user("alice", 501);
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let windowmanager = "alice$ /usr/bin/defaults export com.apple.WindowManager -";
        let notificationcenter = "alice$ /usr/bin/defaults export com.apple.notificationcenterui -";
        for _ in 0..4 {
            sys.on(windowmanager, export("<dict><key>StandardHideWidgets</key><false/><key>StageManagerHideWidgets</key><false/></dict>"));
        }
        sys.on(windowmanager, export("<dict><key>StandardHideWidgets</key><true/><key>StageManagerHideWidgets</key><true/></dict>"));
        for _ in 0..2 {
            sys.on(notificationcenter, export("<dict><key>widgets</key><dict><key>vers</key><integer>1</integer><key>instances</key><array><integer>42</integer></array></dict></dict>"));
        }
        sys.on(notificationcenter, export("<dict><key>widgets</key><dict><key>vers</key><integer>1</integer><key>instances</key><array/></dict></dict>"));
        for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
            let cmd =
                Target::user("com.apple.WindowManager", key, &who).write_cmd(&PValue::Bool(true));
            sys.on(&cmd.display(), Ran::ok(""));
        }
        let cmd = Target::user("com.apple.notificationcenterui", "widgets", &who).write_cmd(
            &PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ]),
        );
        sys.on(&cmd.display(), Ran::ok(""));
        sys.on(
            "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter",
            Ran::ok("123\n"),
        );
        sys.on(
            "/bin/launchctl print gui/501",
            Ran::exit(2, "injected session lookup failure"),
        );
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on(
            "alice@gui$ /usr/bin/killall -u alice NotificationCenter",
            Ran::ok(""),
        );
        let items: Vec<Box<dyn Item>> = vec![Box::new(Desktop {
            user: who.clone(),
            kind: DesktopKind::Widgets,
            before: Mutex::new(Vec::new()),
        })];
        let c = ctx(std::slice::from_ref(&who));
        let run = |mode| crate::engine::run(&items, mode, &sys, &c, &mut crate::engine::NoJournal);
        let first = run(crate::model::Mode::Apply);
        assert_eq!(first[0].outcome, crate::model::Outcome::Failed);
        assert!(crate::init_activation::pending(&sys, &who, "widgets").unwrap());
        let calls = sys.calls();
        let marker = calls.iter().position(|call| call.contains("write /Users//alice/Library/Application Support/aterm/init-activation/widgets.pending")).unwrap();
        let write = calls
            .iter()
            .position(|call| call.contains("/usr/bin/defaults write"))
            .unwrap();
        assert!(marker < write);
        let preference_writes = calls
            .iter()
            .filter(|call| call.contains("/usr/bin/defaults write"))
            .count();
        assert_eq!(preference_writes, 3);
        let checked = run(crate::model::Mode::Plan);
        assert_eq!(checked[0].outcome, crate::model::Outcome::Planned);
        let recovered = run(crate::model::Mode::Apply);
        assert_eq!(recovered[0].outcome, crate::model::Outcome::Changed);
        assert!(!crate::init_activation::pending(&sys, &who, "widgets").unwrap());
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("/usr/bin/defaults write"))
                .count(),
            preference_writes
        );
        assert_eq!(
            calls.iter().filter(|call| call.contains("killall")).count(),
            1
        );

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
            sys.on(
                "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter",
                Ran::exit(1, ""),
            );
            sys.durable_dir_fails(Path::new(directory), Some(std::io::ErrorKind::Other));
            let item = Desktop {
                user: who.clone(),
                kind: DesktopKind::Widgets,
                before: Mutex::new(Vec::new()),
            };
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
            assert!(!crate::init_activation::pending(&sys, &who, "widgets").unwrap());
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
}
