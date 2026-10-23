// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local `aterm init` desktop preferences.

use crate::items::catalog::{CATALOG, CatalogRow, Lit};
use crate::items::defaults::{self, Target};
use crate::items::{Activation, Ctx, Item, keyboard, widgets};
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::plist::Entries;
use crate::port::{Cmd, System};
use aterm_types::macos_setup::{Selection, Setting};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const SRC: &str = "HIDKeyboardModifierMappingSrc";
const WINDOW_MANAGER: &str = "com.apple.WindowManager";
const FINDER: &str = "com.apple.finder";
const DOCK: &str = "com.apple.dock";
const DST: &str = "HIDKeyboardModifierMappingDst";
const HIDUTIL: &str = "/usr/bin/hidutil";
/// The apps the Dock pins, in order, each at its path, with its bundle identifier and label.
/// aterm's path is its usual one: [`dock_apps_available`] pins the copy this process runs
/// from when aterm is in both `/Applications` and the account's `~/Applications`, else the
/// one copy there is ([`aterm_candidates`]).
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
    let activation = Arc::new(DesktopActivation::new(
        user,
        Reach::all(ctx.facts.full_disk_access),
    ));
    for (setting, kind) in desktop() {
        if selected.contains(setting) {
            items.push(Box::new(Desktop::new(user, kind, &activation)));
        }
    }
    Ok(items)
}

/// The setup row of the desktop item with `id` (`consumer.widgets.notification-center@alice`:
/// the widgets row), for a caller that recorded the items a pass withheld
/// ([`Item::withheld`]) and follows them up ([`withheld_items`]); an id an older aterm
/// recorded is read through [`current_ids`] first.
#[must_use]
pub fn setting_of(id: &str) -> Option<Setting> {
    let (name, _) = id.split_once('@')?;
    desktop()
        .find(|(_, kind)| kind.name() == name)
        .map(|(setting, _)| setting)
}

/// The items of this build that `id`, from a record an older aterm wrote, stands for. aterm
/// 0.118.0 and older ran one widgets item, `init.widgets@<user>`, whose keys are now each a
/// `[consumer] widgets` row's own item; `withheld` keeps only those a pass leaves for want of
/// Full Disk Access (Notification Center's), the one key a record of withheld items could
/// owe. Any other id stands for itself.
#[must_use]
pub fn current_ids(id: &str, withheld: bool) -> Vec<String> {
    match id.split_once('@') {
        Some((LEGACY_WIDGETS_ITEM, user)) => desktop()
            .filter_map(|(_, kind)| match kind {
                DesktopKind::Widget(row) if !withheld || defaults::contained(row.domain) => {
                    Some(format!("{}@{user}", row.id))
                }
                _ => None,
            })
            .collect(),
        _ => vec![id.to_owned()],
    }
}

/// The id, less `@<user>`, of the one widgets item aterm 0.118.0 and older ran
/// ([`current_ids`]).
const LEGACY_WIDGETS_ITEM: &str = "init.widgets";

/// The desktop rows, each with its setup row, in run order: the `[consumer] widgets`
/// catalog rows, one key each under the catalog's id (one key, one owner: machines design
/// §5.1), then the desktop and the Dock.
fn desktop() -> impl Iterator<Item = (Setting, DesktopKind)> {
    CATALOG
        .iter()
        .filter(|row| row.preset == "widgets")
        .map(|row| (Setting::Widgets, DesktopKind::Widget(row)))
        .chain([
            (Setting::Desktop, DesktopKind::Desktop),
            (Setting::Dock, DesktopKind::Dock),
        ])
}

/// The follow-up of a pass that left keys in another app's container alone ([`Item::withheld`]).
///
/// # Errors
/// An absent or ambiguous user.
pub fn withheld_items(ctx: &Ctx, selected: Selection) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init preferences require exactly one local user".into());
    };
    let activation = Arc::new(DesktopActivation::new(
        user,
        Reach {
            full_disk_access: ctx.facts.full_disk_access,
            contained_only: true,
        },
    ));
    Ok(desktop()
        .filter(|(setting, kind)| {
            selected.contains(*setting)
                && kind
                    .keys()
                    .iter()
                    .any(|(domain, _, _)| defaults::contained(domain))
        })
        .map(|(_, kind)| Box::new(Desktop::new(user, kind, &activation)) as Box<dyn Item>)
        .collect())
}

/// Apply Caps Lock as Control to attached HID services without replacing the persistent modifier preferences.
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
        let ran = sys
            .run_complete_stdout(&command)
            .map_err(|e| e.to_string())?;
        if !ran.success() {
            return Err(format!("{}: {}", command.display(), ran.describe()));
        }
        parse_hid_mappings(&ran.out())
    }

    fn set_mappings(&self, sys: &dyn System, mappings: &[(i64, i64)]) -> Result<(), String> {
        let values: Vec<aterm_json::Value> = mappings
            .iter()
            .map(|(src, dst)| aterm_json::json!({SRC: src, DST: dst}))
            .collect();
        let value = aterm_json::to_string(&aterm_json::json!({"UserKeyMapping": values}))
            .map_err(|e| e.to_string())?;
        run(sys, &self.command().args(["--set", &value]))?;
        Ok(())
    }
}

/// Remove only Caps Lock's live mapping for a verifier's OFF stage, retaining every
/// other mapping. Unknown or conflicting service state cannot authorize a write.
pub(crate) fn clear_attached_caps_lock(
    sys: &dyn System,
    user: &User,
    model: keyboard::Model,
) -> Result<(), String> {
    let item = AttachedKeyboard {
        model,
        user: user.clone(),
    };
    let maps = item.mappings(sys)?;
    let want = common_other_mappings(&maps)?;
    if maps.iter().any(|map| *map != want) {
        item.set_mappings(sys, &want)?;
    }
    let readback = item.mappings(sys)?;
    if readback.is_empty() || readback.iter().any(|map| *map != want) {
        return Err("Caps Lock OFF did not read back with the other mappings preserved".into());
    }
    Ok(())
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
            self.set_mappings(sys, &want)?;
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
    let mut want = common_other_mappings(maps)?;
    want.push((keyboard::CAPS_LOCK, keyboard::LEFT_CONTROL));
    Ok(Some(want))
}

fn common_other_mappings(maps: &[Vec<(i64, i64)>]) -> Result<Vec<(i64, i64)>, String> {
    let Some(first) = maps.first() else {
        return Err("the attached keyboard has no HID service; left untouched".into());
    };
    let others = |map: &Vec<(i64, i64)>| {
        map.iter()
            .copied()
            .filter(|(s, _)| *s != keyboard::CAPS_LOCK)
            .collect::<Vec<_>>()
    };
    let want = others(first);
    if maps.iter().skip(1).any(|map| others(map) != want) {
        return Err("keyboard services have different unrelated remaps; left untouched".into());
    }
    Ok(want)
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

/// A desktop item's kind: one `[consumer] widgets` catalog row's key, or a family of keys.
#[derive(Debug, Clone, Copy)]
enum DesktopKind {
    /// One widgets row of the catalog ([`desktop`]): its key, under its id.
    Widget(&'static CatalogRow),
    Desktop,
    Dock,
}

impl PartialEq for DesktopKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Widget(a), Self::Widget(b)) => a.id == b.id,
            (Self::Desktop, Self::Desktop) | (Self::Dock, Self::Dock) => true,
            _ => false,
        }
    }
}

impl Eq for DesktopKind {}

/// Which keys of its families a pass reads and writes ([`DesktopKind::reached`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reach {
    /// What the pass measured of its Full Disk Access: without a grant known held, no key
    /// in another app's container ([`defaults::withheld`]).
    full_disk_access: Option<bool>,
    /// A follow-up ([`withheld_items`]): only the keys in another app's container.
    contained_only: bool,
}

impl Reach {
    /// Every key the grant `full_disk_access` lets a pass reach.
    const fn all(full_disk_access: Option<bool>) -> Self {
        Self {
            full_disk_access,
            contained_only: false,
        }
    }

    /// Whether the pass reads and writes `domain`'s keys.
    fn reaches(self, domain: &str) -> bool {
        defaults::withheld(domain, self.full_disk_access).is_none()
            && (!self.contained_only || defaults::contained(domain))
    }
}

/// The activation marker that records a `kind` item's write to `domain`: the family's own;
/// for the widgets' Window Manager keys, [`crate::init_activation::WIDGETS_MARKER`]; for
/// another app's container, [`crate::init_activation::CONTAINED_MARKER`].
fn marker(kind: DesktopKind, domain: &str) -> &'static str {
    if defaults::contained(domain) {
        crate::init_activation::CONTAINED_MARKER
    } else if matches!(kind, DesktopKind::Widget(_)) {
        crate::init_activation::WIDGETS_MARKER
    } else {
        kind.family()
    }
}

/// What a desktop key should hold.
#[derive(Debug, Clone, Copy)]
enum Wanted {
    Bool(bool),
    /// An empty array: no stack, no recent app.
    Empty,
    /// The [`DOCK_APPS`] pins, aterm where it is installed ([`dock_apps_available`]), as the
    /// Dock rewrites them ([`same_apps`]).
    DockApps,
    /// Notification Center's widget list with no widget ([`widgets::cleared`]).
    NoWidgets,
    /// A catalog row's value ([`Lit::value`]).
    Value(Lit),
}

impl Wanted {
    /// What a catalog row's `off` asks for.
    const fn of(lit: Lit) -> Self {
        match lit {
            Lit::NoWidgets => Self::NoWidgets,
            lit => Self::Value(lit),
        }
    }
}

impl DesktopKind {
    /// The family's journal name, and the marker of its keys but the widgets' ([`marker`]).
    const fn family(self) -> &'static str {
        match self {
            Self::Widget(_) => "widgets",
            Self::Desktop => "desktop",
            Self::Dock => "dock",
        }
    }

    /// The id of an item of this kind, less `@<user>`: a widgets row's is its catalog id.
    fn name(self) -> String {
        match self {
            Self::Widget(row) => row.id.to_owned(),
            Self::Desktop | Self::Dock => format!("init.{}", self.family()),
        }
    }

    /// The id of `user`'s item of this kind.
    fn id(self, user: &User) -> String {
        format!("{}@{}", self.name(), user.name)
    }

    /// The processes that read the keys `marker` records of those the family reaches, in
    /// relaunch order: those of each domain they are in ([`crate::init_activation::readers`]).
    fn readers(self, reach: Reach, marker: &str) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| {
                self.recorded(reach, marker)
                    .iter()
                    .any(|(domain, _, _)| crate::init_activation::readers(domain).contains(process))
            })
            .collect()
    }

    /// The keys of [`DesktopKind::keys`] a pass reads and writes ([`Reach::reaches`]): all
    /// of them, less any in another app's container while the grant is not known held
    /// ([`defaults::withheld`]); in a follow-up, those alone.
    fn reached(self, reach: Reach) -> Vec<Key> {
        self.keys()
            .into_iter()
            .filter(|(domain, _, _)| reach.reaches(domain))
            .collect()
    }

    /// The keys the family reaches whose writes `marker` records.
    fn recorded(self, reach: Reach, marker: &str) -> Vec<Key> {
        self.reached(reach)
            .into_iter()
            .filter(|(domain, _, _)| self::marker(self, domain) == marker)
            .collect()
    }

    /// The markers of the keys the family reaches, each once, in key order: a pass begins,
    /// discharges and reports only these.
    fn markers(self, reach: Reach) -> Vec<&'static str> {
        let mut markers = Vec::new();
        for (domain, _, _) in self.reached(reach) {
            let marker = marker(self, domain);
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
        markers
    }

    /// Why each domain of the family's keys the pass leaves alone is left alone, each once.
    fn withheld(self, full_disk_access: Option<bool>) -> Vec<String> {
        let mut why: Vec<String> = Vec::new();
        for (domain, _, _) in self.keys() {
            if let Some(line) = defaults::withheld(domain, full_disk_access)
                && !why.contains(&line)
            {
                why.push(line);
            }
        }
        why
    }

    /// Every key the family may write, in the order it reads them, and what each should
    /// hold: what its item changes and what its activation waits to read the same, of those
    /// a pass reaches ([`DesktopKind::reached`]). A widgets row's is its catalog row's key.
    fn keys(self) -> Vec<Key> {
        let keys: &[Key] = match self {
            Self::Widget(row) => return vec![(row.domain, row.key, Wanted::of(row.off))],
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
        };
        keys.to_vec()
    }
}

/// A desktop key: its domain, its name, and what it should hold.
type Key = (&'static str, &'static str, Wanted);

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

    /// Which keys the pass reaches, which its activation holds for every family.
    fn reach(&self) -> Reach {
        self.activation.reach
    }

    /// The pass's measured Full Disk Access.
    fn full_disk_access(&self) -> Option<bool> {
        self.reach().full_disk_access
    }

    /// Whether an earlier write of the keys this pass reaches still awaits its readers'
    /// relaunch: a marker of those keys is pending, or an older aterm's marker of every
    /// widgets key ([`Desktop::legacy`]). A marker this pass found clear and another of its
    /// items then began is the pass's own relaunch to make, not an earlier write
    /// ([`DesktopActivation::earlier`]): the two Window Manager widgets rows share one.
    fn pending(&self, sys: &dyn System) -> Result<bool, String> {
        for marker in self
            .kind
            .markers(self.reach())
            .into_iter()
            .chain(self.legacy())
        {
            if self.activation.earlier(sys, marker)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The marker an older aterm wrote before every widgets write, Notification Center's with
    /// the Window Manager's ([`crate::init_activation::LEGACY_WIDGETS_MARKER`]), for a
    /// widgets item of a Window Manager key in a pass that reaches it (not a follow-up,
    /// which never relaunches the Dock): the first of them carries it.
    fn legacy(&self) -> Option<&'static str> {
        let wm = matches!(self.kind, DesktopKind::Widget(row) if !defaults::contained(row.domain));
        (wm && !self.reach().contained_only)
            .then_some(crate::init_activation::LEGACY_WIDGETS_MARKER)
    }

    /// Carry what an older aterm's pending widgets marker owes to the two markers that now
    /// record those writes, and drop it: the Dock's ([`crate::init_activation::WIDGETS_MARKER`])
    /// and Notification Center's ([`crate::init_activation::CONTAINED_MARKER`]), each written
    /// before the old one goes, so a pass that ends between them carries it again.
    fn carry_legacy(&self, sys: &dyn System) -> Result<(), String> {
        let Some(legacy) = self.legacy() else {
            return Ok(());
        };
        if !crate::init_activation::pending(sys, &self.user, legacy)? {
            return Ok(());
        }
        for marker in [
            crate::init_activation::WIDGETS_MARKER,
            crate::init_activation::CONTAINED_MARKER,
        ] {
            crate::init_activation::begin(sys, &self.user, marker)?;
            self.activation.carried(marker);
        }
        crate::init_activation::finish(sys, &self.user, legacy)
    }

    /// What the item reads as wanted: its title, and why any key it leaves alone is.
    fn summary(&self) -> String {
        let withheld = self.kind.withheld(self.full_disk_access());
        if withheld.is_empty() {
            self.title()
        } else {
            format!("{} ({})", self.title(), withheld.join("; "))
        }
    }

    fn changes(&self, sys: &dyn System) -> Result<Changes, String> {
        let apps = if self.kind == DesktopKind::Dock {
            dock_apps_available(sys, &self.user)?
        } else {
            Vec::new()
        };
        let mut changes = Vec::new();
        for (domain, key, wanted) in self.kind.reached(self.reach()) {
            let target = self.target(domain, key);
            let read = target.read(sys);
            let want = match wanted {
                Wanted::NoWidgets => widgets::cleared(sys, &self.user, &read)?,
                Wanted::DockApps => unmet(&read, dock_apps(&apps), |now, want| {
                    same_apps(sys, now, want)
                })?,
                Wanted::Empty => unmet(&read, PValue::Array(Vec::new()), equivalent)?,
                Wanted::Bool(value) => unmet(&read, PValue::Bool(value), equivalent)?,
                Wanted::Value(value) => unmet(&read, value.value(), equivalent)?,
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
#[derive(Debug)]
struct DesktopActivation {
    user: User,
    /// Which keys its families reach ([`DesktopKind::reached`]), from the Full Disk Access
    /// the pass measured, and so which markers, readers and looks are the pass's.
    reach: Reach,
    /// The families whose items started writing in this pass, in run order.
    owed: Mutex<Vec<DesktopKind>>,
    /// Each marker the pass's items looked at, and whether it was pending at the first
    /// look, until the pass's relaunch discharges it ([`DesktopActivation::earlier`]).
    seen: Mutex<Vec<(&'static str, bool)>>,
}

/// One relaunch a pass owes: a family, and the marker of the keys it wrote.
type Obligation = (DesktopKind, &'static str);

impl DesktopActivation {
    fn new(user: &User, reach: Reach) -> Self {
        Self {
            user: user.clone(),
            reach,
            owed: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Whether `marker` holds a write of an earlier pass still awaiting its readers'
    /// relaunch: pending now, and already when this pass first looked. One the pass found
    /// clear and an item of it then began is this pass's to discharge, so another item
    /// that shares it (the two Window Manager widgets rows) reads its own key alone.
    fn earlier(&self, sys: &dyn System, marker: &'static str) -> Result<bool, String> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        match seen.iter().find(|(looked, _)| *looked == marker) {
            Some(&(_, false)) => Ok(false),
            Some(&(_, true)) => crate::init_activation::pending(sys, &self.user, marker),
            None => {
                let pending = crate::init_activation::pending(sys, &self.user, marker)?;
                seen.push((marker, pending));
                Ok(pending)
            }
        }
    }

    /// `marker` now carries what an older aterm's marker owed ([`Desktop::carry_legacy`]):
    /// an earlier write, for every item that shares it.
    fn carried(&self, marker: &'static str) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.retain(|(looked, _)| *looked != marker);
        seen.push((marker, true));
    }

    /// `kind`'s marker is written and its writes begin: this pass relaunches its readers.
    fn owe(&self, kind: DesktopKind) {
        let mut owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
        if !owed.contains(&kind) {
            owed.push(kind);
        }
    }

    /// What this pass owes a relaunch for whose marker is still pending: of each family it
    /// owes, each marker of the keys it reaches ([`DesktopKind::markers`]).
    fn obligations(&self, sys: &dyn System) -> Result<Vec<Obligation>, String> {
        let owed = self
            .owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut due = Vec::new();
        for kind in owed {
            for marker in kind.markers(self.reach) {
                if crate::init_activation::pending(sys, &self.user, marker)? {
                    due.push((kind, marker));
                }
            }
        }
        Ok(due)
    }

    /// The processes that read the keys `due` records, each once, in relaunch order.
    fn consumers(&self, due: &[Obligation]) -> Vec<&'static str> {
        crate::init_activation::RELAUNCHED
            .into_iter()
            .filter(|process| {
                due.iter()
                    .any(|(kind, marker)| kind.readers(self.reach, marker).contains(process))
            })
            .collect()
    }

    /// What `process` reads of what `due` records, as it now holds it
    /// ([`crate::init_activation::reading`]): `Err` while it is the Dock and has not yet
    /// loaded the tiles it was handed.
    fn look(
        &self,
        sys: &dyn System,
        due: &[Obligation],
        process: &str,
    ) -> Result<Vec<Result<Entries, String>>, String> {
        let keys: Vec<Target> = due
            .iter()
            .flat_map(|&(kind, marker)| kind.recorded(self.reach, marker))
            .filter(|(domain, _, _)| crate::init_activation::readers(domain).contains(&process))
            .map(|(domain, key, _)| Target::user(domain, key, &self.user))
            .collect();
        crate::init_activation::reading(sys, &keys)
    }
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
        let due = self.obligations(sys)?;
        if due.is_empty() {
            return Ok(None);
        }
        let processes: Vec<String> = self
            .consumers(&due)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut writers: Vec<String> = Vec::new();
        for (kind, _) in &due {
            let id = kind.id(&self.user);
            if !writers.contains(&id) {
                writers.push(id);
            }
        }
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

    /// Relaunch the readers of every marker owed, once each, and wait for each; then
    /// clear each marker all of whose keys' readers are up, and name the items of the
    /// others, each with why its readers are not.
    fn activate(&self, sys: &dyn System, _ctx: &Ctx) -> Result<Vec<(String, String)>, String> {
        let due = self.obligations(sys)?;
        let processes = self.consumers(&due);
        let answers =
            crate::init_activation::relaunch_settled(sys, &self.user, &processes, |process| {
                self.look(sys, &due, process)
            });
        let mut failed: Vec<(String, String)> = Vec::new();
        for &(kind, marker) in &due {
            let readers = kind.readers(self.reach, marker);
            let why: Vec<&str> = processes
                .iter()
                .zip(&answers)
                .filter(|(process, _)| readers.contains(process))
                .filter_map(|(_, answer)| answer.as_ref().err().map(String::as_str))
                .collect();
            let finished = if why.is_empty() {
                crate::init_activation::finish(sys, &self.user, marker)
            } else {
                Err(why.join("; "))
            };
            if let Err(why) = finished {
                let id = kind.id(&self.user);
                match failed.iter_mut().find(|(failed, _)| *failed == id) {
                    Some((_, all)) if !all.contains(&why) => {
                        all.push_str("; ");
                        all.push_str(&why);
                    }
                    Some(_) => {}
                    None => failed.push((id, why)),
                }
            }
        }
        self.owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|kind| !due.iter().any(|(owed, _)| owed == kind));
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(looked, _)| !due.iter().any(|(_, marker)| marker == looked));
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
            DesktopKind::Widget(row)
                if !self.reach().contained_only
                    && !self.kind.withheld(self.full_disk_access()).is_empty() =>
            {
                format!("No {}; they wait for Full Disk Access", row.what)
            }
            DesktopKind::Widget(row) => format!("No {}", row.what),
            DesktopKind::Desktop => {
                "Desktop icons visible; clicking wallpaper keeps windows in place".into()
            }
            DesktopKind::Dock => {
                "Dock pins: aterm, System Settings, Safari; no stacks or recent apps".into()
            }
        }
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let pending = match self.pending(sys) {
            Ok(pending) => pending,
            Err(why) => return Probe::Unusable { why },
        };
        match self.changes(sys) {
            Err(why) => Probe::Unusable { why },
            Ok(changes) if changes.is_empty() && !pending => Probe::Ok {
                now: self.summary(),
            },
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
                        to: self.summary(),
                    }
                } else {
                    Probe::Deferred { why: "relaunches the user's desktop process; use the local init command at the console".into() }
                }
            }
        }
    }

    /// Write the family's keys, after the durable marker of each key written ([`marker`]),
    /// and of each already pending (an older aterm's widgets marker carried into them first,
    /// [`Desktop::carry_legacy`]).
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.carry_legacy(sys)?;
        let changes = self.changes(sys)?;
        let mut owed = false;
        for marker in self.kind.markers(self.reach()) {
            let writes = changes
                .iter()
                .any(|(target, _, _)| self::marker(self.kind, &target.domain) == marker);
            if writes || crate::init_activation::pending(sys, &self.user, marker)? {
                crate::init_activation::begin(sys, &self.user, marker)?;
                owed = true;
            }
        }
        if !owed {
            return Ok(());
        }
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

    fn withheld(&self) -> Option<String> {
        let why = self.kind.withheld(self.full_disk_access());
        (!why.is_empty()).then(|| why.join("; "))
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

/// The pins of `apps` (each a path, a bundle identifier and a label), in order.
fn dock_apps(apps: &[DockApp]) -> PValue {
    PValue::Array(
        apps.iter()
            .map(|&(ref path, bundle, label)| {
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
                                    ("_CFURLString".into(), PValue::Str(file_url(path))),
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

/// `path` as the directory URL the Dock stores (`_CFURLStringType` 15): each byte outside
/// RFC 3986's path characters percent-encoded, so [`dock_app_path`] reads back the same
/// path whatever a home folder is named (`#`, `?`, `%`, a space, any non-ASCII letter).
fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url.push('/');
    url
}

/// One app the Dock pins: its path, bundle identifier and label.
type DockApp = (String, &'static str, &'static str);

/// Where the Dock looks for aterm, in order: `/Applications`, where `tools/install.sh` puts
/// it, then the account's own `~/Applications`, where it goes when `/Applications` is not
/// writable (a standard account). [`aterm_candidates`] puts the running copy first.
fn aterm_paths(user: &User) -> [String; 2] {
    [
        DOCK_APPS[0].0.to_owned(),
        format!("{}/Applications/aterm.app", user.home.trim_end_matches('/')),
    ]
}

/// [`aterm_paths`], the one this process runs from first ([`System::running_app`]): with
/// aterm in both folders, the copy this account runs and its updater keeps current is the
/// one its Dock opens, not another account's, which may be old.
fn aterm_candidates(sys: &dyn System, user: &User) -> Vec<String> {
    let mut paths = aterm_paths(user).to_vec();
    let canonical = |path: &str| {
        sys.canonicalize(Path::new(path))
            .unwrap_or_else(|_| path.into())
    };
    if let Some(running) = sys.running_app()
        && let Some(at) = paths.iter().position(|path| canonical(path) == running)
    {
        let own = paths.remove(at);
        paths.insert(0, own);
    }
    paths
}

/// The [`DOCK_APPS`] as installed, aterm at the first of [`aterm_candidates`] that holds it.
/// An asserted bundle identifier in Dock preferences does not establish an installed app,
/// so each is read from its bundle.
///
/// # Errors
/// An app is missing, unreadable, or another app: the Dock is left untouched.
fn dock_apps_available(sys: &dyn System, user: &User) -> Result<Vec<DockApp>, String> {
    let [(_, aterm, label), others @ ..] = DOCK_APPS;
    let mut apps = Vec::with_capacity(DOCK_APPS.len());
    let mut why = Vec::new();
    for path in aterm_candidates(sys, user) {
        match installed_app(sys, user, &path, aterm, label) {
            Ok(()) => {
                apps.push((path, aterm, label));
                break;
            }
            Err(Missing::NotInstalled) => why.push((true, format!("nothing at {path}"))),
            Err(Missing::Other(line)) => why.push((false, line)),
        }
    }
    if apps.is_empty() {
        let [usual, own] = aterm_paths(user);
        return Err(if why.iter().all(|(missing, _)| *missing) {
            format!("{label} is not installed at {usual} or {own}; Dock left untouched")
        } else {
            let why: Vec<String> = why.into_iter().map(|(_, line)| line).collect();
            format!(
                "no {label} to pin ({}); Dock left untouched",
                why.join(", ")
            )
        });
    }
    for (path, bundle, label) in others {
        installed_app(sys, user, path, bundle, label).map_err(|missing| {
            let why = match missing {
                Missing::NotInstalled => format!("{label} is not installed at {path}"),
                Missing::Other(line) => line,
            };
            format!("{why}; Dock left untouched")
        })?;
    }
    apps.extend(
        others
            .into_iter()
            .map(|(path, bundle, label)| (path.to_owned(), bundle, label)),
    );
    Ok(apps)
}

/// Why an app the Dock would pin is not there to pin.
enum Missing {
    /// Nothing is at its path.
    NotInstalled,
    /// Its bundle cannot be read, or is another app; the line says which.
    Other(String),
}

/// Whether `path` holds the app `bundle`, read as `user` from its bundle's Info.plist.
fn installed_app(
    sys: &dyn System,
    user: &User,
    path: &str,
    bundle: &str,
    label: &str,
) -> Result<(), Missing> {
    let contents = Path::new(path).join("Contents");
    if !sys.stat(&contents).is_ok_and(|s| s.dir) {
        return Err(Missing::NotInstalled);
    }
    let cmd = Cmd::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
        .arg(contents.join("Info.plist"))
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|_| Missing::Other(format!("cannot verify {label} at {path}")))?;
    if !ran.success() || ran.out().trim() != bundle {
        return Err(Missing::Other(format!(
            "{path} is not the requested {label} application"
        )));
    }
    Ok(())
}

/// Dock adds bookmarks and GUIDs and changes URL escaping or trailing separators.
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

    /// The marker of the widgets' Window Manager keys.
    const WIDGETS: &str = crate::init_activation::WIDGETS_MARKER;

    /// Notification Center's preferences, in its app container.
    const NOTIFICATION_CENTER: &str = "com.apple.notificationcenterui";

    /// A desktop item of its own pass, whose process holds Full Disk Access (the arm that
    /// reaches Notification Center's container).
    fn desktop(who: &User, kind: DesktopKind) -> Desktop {
        Desktop::new(
            who,
            kind,
            &Arc::new(DesktopActivation::new(who, Reach::all(Some(true)))),
        )
    }

    /// The [`DOCK_APPS`] at their usual paths, aterm in `/Applications`.
    fn usual_dock_apps() -> Vec<DockApp> {
        DOCK_APPS
            .into_iter()
            .map(|(path, bundle, label)| (path.to_owned(), bundle, label))
            .collect()
    }

    fn installed_dock_apps(sys: &Fake, who: &User) {
        for (path, bundle, _) in DOCK_APPS {
            installed_at(sys, who, path, bundle);
        }
    }

    /// The app `bundle` at `path`, as `who` reads its Info.plist.
    fn installed_at(sys: &Fake, who: &User, path: &str, bundle: &str) {
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
        let _ = item.mappings(&sys).unwrap();
        assert!(matches!(item.probe(&sys, &c), Probe::Ok { .. }));

        let before = "RegistryID Key Value\n10002160e UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; }, { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n100021604 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; }, { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n";
        let after = "RegistryID Key Value\n10002160e UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; } )\n100021604 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; } )\n";
        let off = Fake::root();
        off.on(&get, Ran::ok(before)).on(&get, Ran::ok(after));
        let set_others = item.command().args(["--set", "{\"UserKeyMapping\":[{\"HIDKeyboardModifierMappingDst\":456,\"HIDKeyboardModifierMappingSrc\":123}]}"]).display();
        off.on(&set_others, Ran::ok(""));
        clear_attached_caps_lock(&off, &item.user, item.model).unwrap();
        assert!(off.calls().contains(&set_others));
        assert!(
            off.calls()
                .iter()
                .all(|call| call.contains("PrimaryUsagePage") && call.contains("PrimaryUsage"))
        );

        for unknown in [
            "RegistryID Key Value\n",
            "10002160e UserKeyMapping (garbage)",
            "10002160e UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 123; HIDKeyboardModifierMappingDst = 456; } )\n100021604 UserKeyMapping (null)\n",
        ] {
            let off = Fake::root();
            off.on(&get, Ran::ok(unknown));
            assert!(clear_attached_caps_lock(&off, &item.user, item.model).is_err());
            assert!(!off.calls().iter().any(|call| call.contains("--set")));
        }
        let refused = Fake::root();
        refused.on(&get, Ran::ok(before));
        refused.refuse_complete_stdout(&get, "incomplete service inventory");
        assert!(clear_attached_caps_lock(&refused, &item.user, item.model).is_err());
        assert!(!refused.calls().iter().any(|call| call.contains("--set")));

        let unchanged = Fake::root();
        unchanged
            .on(&get, Ran::ok(before))
            .on(&set_others, Ran::ok(""));
        assert!(clear_attached_caps_lock(&unchanged, &item.user, item.model).is_err());
        assert!(unchanged.calls().contains(&set_others));
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
    fn dock_metadata_is_ignored_but_pin_order_extra_pins_and_stacks_are_not() {
        let want = dock_apps(&usual_dock_apps());
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
        let want = dock_apps(&usual_dock_apps());
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

    /// An app the Dock pins beside aterm that is missing, or another app at its path, leaves
    /// the Dock as it was and says which, on a Dock the row can read. Negative controls:
    /// `installed_app` accepting any bundle identifier pins the impostor; skipping the
    /// other apps' check in `dock_apps_available` pins the missing Safari.
    #[test]
    fn missing_or_wrong_installed_dock_apps_cannot_be_reported_configured() {
        let who = user("alice", 501);
        let safari = DOCK_APPS[2].0;
        for (identity, expected) in [
            (None, format!("Safari is not installed at {safari}")),
            (
                Some("com.example.another-app"),
                format!("{safari} is not the requested Safari application"),
            ),
        ] {
            let (sys, desk) = desktop_with(&who, Desk::before(), false);
            for (path, bundle, _) in &DOCK_APPS[..2] {
                installed_at(&sys, &who, path, bundle);
            }
            if let Some(bundle) = identity {
                installed_at(&sys, &who, safari, bundle);
            }
            let run = pass(
                &sys,
                selected_desktop_items(&who, &[Setting::Dock]),
                Mode::Apply,
            );
            assert_eq!(run.rows[0].outcome, Outcome::Unknown, "{:?}", run.rows);
            assert_eq!(
                run.rows[0].why.as_deref(),
                Some(format!("{expected}; Dock left untouched").as_str())
            );
            let desk = desk.lock().unwrap();
            assert!(
                !desk
                    .events
                    .iter()
                    .any(|e| e.starts_with("write ") || e.starts_with("kill ")),
                "{:?}",
                desk.events
            );
        }
    }

    /// aterm in the account's `~/Applications` (`tools/install.sh` on a standard account,
    /// where `/Applications` is not writable) is the aterm the Dock pins, also when
    /// `/Applications/aterm.app` is some other app; with aterm in both, the copy this process
    /// runs from, else `/Applications`'s; with neither, the row says what each folder holds
    /// and leaves the Dock alone. Negative controls: `aterm_paths` without the home entry
    /// leaves the first two Unknown; `aterm_candidates` without the running copy's move pins
    /// `/Applications` in the fourth.
    #[test]
    fn the_dock_pins_the_aterm_in_the_accounts_own_applications_folder() {
        let who = user("alice", 501);
        let usual_path = DOCK_APPS[0].0;
        let own = "/Users//alice/Applications/aterm.app";
        let pins_at = |aterm: &str| {
            let mut apps = vec![(aterm.to_owned(), DOCK_APPS[0].1, DOCK_APPS[0].2)];
            apps.extend(
                DOCK_APPS[1..]
                    .iter()
                    .map(|&(path, bundle, label)| (path.to_owned(), bundle, label)),
            );
            dock_apps(&apps)
        };
        let aterm = Some("com.aterm.aterm");
        // (aterm at /Applications, aterm at ~/Applications, the running copy, the copy pinned
        // or what the row says)
        for (usual, home, running, pinned) in [
            (None, aterm, None, Ok(own)),
            (Some("com.example.another-app"), aterm, None, Ok(own)),
            (aterm, aterm, None, Ok(usual_path)),
            (aterm, aterm, Some(own), Ok(own)),
            (aterm, aterm, Some(usual_path), Ok(usual_path)),
            (aterm, None, Some(own), Ok(usual_path)),
            (
                None,
                None,
                None,
                Err(format!("aterm is not installed at {usual_path} or {own}")),
            ),
            (
                Some("com.example.another-app"),
                None,
                None,
                Err(format!(
                    "no aterm to pin ({usual_path} is not the requested aterm application, \
                     nothing at {own})"
                )),
            ),
            (
                None,
                Some("com.aterm.aterm.dev"),
                Some(own),
                Err(format!(
                    "no aterm to pin ({own} is not the requested aterm application, nothing at \
                     {usual_path})"
                )),
            ),
        ] {
            let (mut sys, desk) = desktop_with(&who, Desk::before(), false);
            sys.running_app = running.map(PathBuf::from);
            for (path, bundle, _) in &DOCK_APPS[1..] {
                installed_at(&sys, &who, path, bundle);
            }
            if let Some(bundle) = usual {
                installed_at(&sys, &who, usual_path, bundle);
            }
            if let Some(bundle) = home {
                installed_at(&sys, &who, own, bundle);
            }
            let run = pass(
                &sys,
                selected_desktop_items(&who, &[Setting::Dock]),
                Mode::Apply,
            );
            let case = format!("{usual:?} {home:?} {running:?}");
            match pinned {
                Ok(aterm) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Changed,
                        "{case}: {:?}",
                        run.rows
                    );
                    let desk = desk.lock().unwrap();
                    let now = desk.read(DOCK, "persistent-apps").unwrap();
                    assert!(same_apps(&sys, now, &pins_at(aterm)), "{case}: {now:?}");
                }
                Err(expected) => {
                    assert_eq!(
                        run.rows[0].outcome,
                        Outcome::Unknown,
                        "{case}: {:?}",
                        run.rows
                    );
                    let why = run.rows[0].why.clone().unwrap_or_default();
                    assert_eq!(why, format!("{expected}; Dock left untouched"), "{case}");
                    assert!(
                        !desk
                            .lock()
                            .unwrap()
                            .events
                            .iter()
                            .any(|e| e.starts_with("write ")),
                        "{case}"
                    );
                }
            }
        }
    }

    /// A pin's URL reads back as the path it was made from, whatever the folder is named,
    /// so the row converges for a home with `#`, `?`, `%`, spaces or non-ASCII letters.
    /// Negative control: escaping only spaces (the old `dock_apps`) loses the `#` and `%`
    /// homes.
    #[test]
    fn a_pin_reads_back_as_the_path_it_was_made_from() {
        for path in [
            "/Applications/aterm.app",
            "/System/Applications/System Settings.app",
            "/Volumes/Work #2/bob/Applications/aterm.app",
            "/Users//what?/Applications/aterm.app",
            "/Volumes/100%/bob/Applications/aterm.app",
            "/Volumes/50%20off/bob/Applications/aterm.app",
            "/Users//zo\u{eb} (old)+new/Applications/aterm.app",
        ] {
            let PValue::Array(pins) = dock_apps(&[(path.to_owned(), "com.aterm.aterm", "aterm")])
            else {
                unreachable!()
            };
            assert_eq!(dock_app_path(&pins[0]), Some(PathBuf::from(path)), "{path}");
        }
        assert_eq!(
            file_url("/System/Applications/System Settings.app"),
            "file:///System/Applications/System%20Settings.app/",
            "the URL the Dock itself writes for a space"
        );
    }

    /// An `~/Applications` that is a link (to a folder on another volume) is matched by
    /// where it leads, as [`System::running_app`] reads the running copy: the account's own
    /// aterm is the running one and is pinned, and a pin spelled through the link's target
    /// is the same app, so the row converges. Negative controls: comparing raw paths in
    /// `aterm_candidates` pins `/Applications`; in `same_apps`, the target's pin reads as
    /// drift.
    #[test]
    fn an_applications_folder_reached_through_a_link_still_pins_the_running_copy() {
        let who = user("alice", 501);
        let own = "/Users//alice/Applications/aterm.app";
        let real = "/Volumes/Apps/alice/aterm.app";
        let pins_at = |aterm: &str| {
            let mut apps = usual_dock_apps();
            apps[0].0 = aterm.to_owned();
            dock_apps(&apps)
        };
        let (mut sys, desk) = desktop_of(&who, Desk::before());
        installed_at(&sys, &who, own, DOCK_APPS[0].1);
        sys.link("/Users//alice/Applications", "/Volumes/Apps/alice");
        sys.running_app = Some(PathBuf::from(real));
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(run.rows[0].outcome, Outcome::Changed, "{:?}", run.rows);
        let desk = desk.lock().unwrap();
        let now = desk.read(DOCK, "persistent-apps").unwrap();
        assert!(same_apps(&Fake::root(), now, &pins_at(own)), "{now:?}");
        assert!(same_apps(&sys, &pins_at(real), &pins_at(own)));
        assert!(!same_apps(&sys, &pins_at(real), &pins_at(DOCK_APPS[0].0)));
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
        let finder = "alice$ /usr/bin/defaults export com.apple.finder -";
        let c = ctx(std::slice::from_ref(&who));
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
            sys.on(windowmanager, export("<dict><key>EnableStandardClickToShowDesktop</key><false/><key>StandardHideDesktopIcons</key><true/><key>StageManagerHideDesktopIcons</key><true/></dict>"));
            sys.on(
                finder,
                export("<dict><key>CreateDesktop</key><true/></dict>"),
            );
            for key in ["StandardHideDesktopIcons", "StageManagerHideDesktopIcons"] {
                let cmd = Target::user("com.apple.WindowManager", key, &who)
                    .write_cmd(&PValue::Bool(false));
                sys.on(&cmd.display(), Ran::ok(""));
            }
            sys.durable_dir_fails(Path::new(directory), Some(std::io::ErrorKind::Other));
            let item = desktop(&who, DesktopKind::Desktop);
            assert!(item.apply(&sys, &c).is_err());
            assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
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
            assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
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
    /// Center, each started again at once when it is killed, as launchd does.
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
        /// A domain macOS keeps from the reader, as it keeps another app's container from a
        /// process without Full Disk Access: its export prints an empty dictionary (measured
        /// 2026-10-22); a write is assumed to exit 0 and never be read back (not measured).
        denied: Option<&'static str>,
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
                denied: None,
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
                ("/usr/bin/defaults", ["export", domain, "-"]) if self.denied == Some(*domain) => {
                    Some(Ran::ok(&crate::plist::document(&PValue::Dict(Vec::new()))))
                }
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
                    if self.denied == Some(*domain) {
                        self.events.push(format!("write {domain} {key}"));
                        return Some(Ran::ok(""));
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
        desktop_with(who, desk, true)
    }

    /// `desk`, run by a fake acting as `who`, the Dock's three apps installed at their usual
    /// paths or none of them (no aterm in either folder, as with only a dev build).
    fn desktop_with(who: &User, desk: Desk, dock_apps: bool) -> (Fake, Arc<Mutex<Desk>>) {
        let mut sys = Fake::root();
        sys.euid = who.uid;
        if dock_apps {
            installed_dock_apps(&sys, who);
        }
        for (_, pid, _) in &desk.running {
            sys.alive(*pid);
        }
        let desk = Arc::new(Mutex::new(desk));
        let model = Arc::clone(&desk);
        sys.simulate(move |fake, cmd| model.lock().unwrap().answer(fake, cmd));
        (sys, desk)
    }

    /// The widget, desktop and Dock items of one pass, as setup selects them, run by a
    /// process that holds Full Disk Access: the arm that reaches Notification Center.
    fn desktop_items(who: &User) -> Vec<Box<dyn Item>> {
        selected_desktop_items(who, &[Setting::Widgets, Setting::Desktop, Setting::Dock])
    }

    /// The items of `settings` of one pass, as setup selects them, with Full Disk Access.
    fn selected_desktop_items(who: &User, settings: &[Setting]) -> Vec<Box<dyn Item>> {
        selected_desktop_items_at(who, Some(true), settings)
    }

    /// The widget, desktop and Dock items of one pass whose process's Full Disk Access is
    /// `full_disk_access`, as the pass measured it.
    fn desktop_items_at(who: &User, full_disk_access: Option<bool>) -> Vec<Box<dyn Item>> {
        selected_desktop_items_at(
            who,
            full_disk_access,
            &[Setting::Widgets, Setting::Desktop, Setting::Dock],
        )
    }

    /// The items of `settings` of one pass, as setup selects them, whose process's Full
    /// Disk Access is `full_disk_access`.
    fn selected_desktop_items_at(
        who: &User,
        full_disk_access: Option<bool>,
        settings: &[Setting],
    ) -> Vec<Box<dyn Item>> {
        let mut selected = Selection::none();
        for &setting in settings {
            selected.set(setting, true);
        }
        let mut context = ctx(std::slice::from_ref(who));
        context.facts.full_disk_access = full_disk_access;
        selected_items(&context, selected).unwrap()
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

    /// The widgets rows' ids: the `[consumer]` catalog's.
    const WIDGET_IDS: [&str; 3] = [
        "consumer.widgets.desktop@alice",
        "consumer.widgets.stage-manager@alice",
        NC_ID,
    ];

    /// The widgets row of Notification Center's list, in its app container.
    const NC_ID: &str = "consumer.widgets.notification-center@alice";

    const DESKTOP_IDS: [&str; 5] = [
        WIDGET_IDS[0],
        WIDGET_IDS[1],
        WIDGET_IDS[2],
        "init.desktop@alice",
        "init.dock@alice",
    ];

    /// Setup's widgets rows are the `[consumer] widgets` catalog's (design §9 A30): with
    /// Full Disk Access, init's widget items carry the ids a fleet baseline's
    /// `widgets = false` expands to for the same account, in the catalog's order, and each
    /// plans its own row's key, so one key has one owner. A pass from a desktop that shows
    /// a widget then reads Notification Center's list back cleared, keeping the field it
    /// does not know. Negative control: init's ids spelled apart from the rows'
    /// (`init.widgets.<key>@alice`) fail the first assertion.
    #[test]
    fn inits_widget_items_are_the_catalogs_rows_and_the_clear_reads_back() {
        let who = user("alice", 501);
        let mut context = ctx(std::slice::from_ref(&who));
        context.facts.full_disk_access = Some(true);
        let fleet = crate::items::catalog::expand(
            &crate::items::catalog::Table {
                widgets: Some(false),
                ..crate::items::catalog::Table::default()
            },
            &context,
        )
        .unwrap();
        let init = selected_desktop_items(&who, &[Setting::Widgets]);
        let ids = |items: &[Box<dyn Item>]| items.iter().map(|item| item.id()).collect::<Vec<_>>();
        assert_eq!(ids(&init), ids(&fleet));
        assert_eq!(ids(&init), WIDGET_IDS);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let keys = |item: &dyn Item| -> Vec<(String, String)> {
            item.preference_changes(&sys, &context)
                .unwrap()
                .into_iter()
                .map(|(target, _)| (target.domain, target.key))
                .collect()
        };
        let rows: Vec<&CatalogRow> = CATALOG
            .iter()
            .filter(|row| row.preset == "widgets")
            .collect();
        assert_eq!(rows.len(), WIDGET_IDS.len());
        for ((row, init), fleet) in rows.iter().zip(&init).zip(&fleet) {
            let own = vec![(row.domain.to_owned(), row.key.to_owned())];
            assert_eq!(keys(init.as_ref()), own, "{}", init.id());
            assert_eq!(keys(fleet.as_ref()), own, "{}", fleet.id());
        }
        desk.lock().unwrap().write(
            NOTIFICATION_CENTER,
            "widgets",
            PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("future".into(), PValue::Bool(true)),
                (
                    "instances".into(),
                    PValue::Array(vec![PValue::Data(vec![42])]),
                ),
            ]),
        );
        let run = pass(&sys, init, Mode::Apply);
        assert_eq!(
            outcomes(&run),
            WIDGET_IDS.map(|id| (id, Outcome::Changed)),
            "{:?}",
            run.rows
        );
        assert!(run.attempts.iter().all(|attempt| attempt.verified));
        let desk = desk.lock().unwrap();
        let Some(PValue::Dict(list)) = desk.read(NOTIFICATION_CENTER, "widgets") else {
            panic!("{:?}", desk.read(NOTIFICATION_CENTER, "widgets"));
        };
        assert!(
            list.contains(&("instances".into(), PValue::Array(Vec::new())))
                && list.contains(&("future".into(), PValue::Bool(true))),
            "{list:?}"
        );
        assert_eq!(
            desk.count("kill NotificationCenter"),
            1,
            "{:?}",
            desk.events
        );
    }

    /// The two Window Manager widgets rows share the Dock's marker: one whose key already
    /// holds what it wants reads Ok beside the other's write, which begins that marker in
    /// the same pass, so it is neither journaled nor reported as a change aterm made, and
    /// the Dock relaunches once for both. A marker an earlier pass left still makes each
    /// row that shares it report the relaunch it awaits. Negative control: a probe that
    /// counts the pass's own marker as an earlier write reads the stage-manager row Changed
    /// and journals it.
    #[test]
    fn a_widgets_row_already_set_reads_ok_beside_one_that_begins_the_shared_marker() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        desk.lock().unwrap().write(
            WINDOW_MANAGER,
            "StageManagerHideWidgets",
            PValue::Bool(true),
        );
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Ok),
                (WIDGET_IDS[2], Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        assert!(
            journaled(&sys).iter().all(|(id, _)| id != WIDGET_IDS[1]),
            "{:?}",
            journaled(&sys)
        );
        assert_eq!(desk.lock().unwrap().count("kill Dock"), 1);
        assert!(!crate::init_activation::pending(&sys, &who, WIDGETS).unwrap());

        let (sys, desk) = desktop_of(&who, Desk::before());
        for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
            desk.lock()
                .unwrap()
                .write(WINDOW_MANAGER, key, PValue::Bool(true));
        }
        crate::init_activation::begin(&sys, &who, WIDGETS).unwrap();
        let run = pass(
            &sys,
            selected_desktop_items(&who, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&run)[..2],
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        for row in &run.rows[..2] {
            assert!(
                row.from
                    .as_deref()
                    .is_some_and(|from| from.contains("after an earlier write")),
                "{row:?}"
            );
        }
        assert_eq!(desk.lock().unwrap().count("kill Dock"), 1);
        assert!(!crate::init_activation::pending(&sys, &who, WIDGETS).unwrap());
    }

    /// The owner's Mac, 2026-10-21 23:01 to 02:24: each unattended pass wrote the desktop
    /// with no aterm the Dock could pin (then any not at `/Applications/aterm.app`; now none
    /// in `/Applications` or `~/Applications`, as with only a dev build): its row reads
    /// Unknown, saying why, and nothing of the Dock is backed up, written or owed, while the
    /// widget and desktop rows still apply through the snapshot guard and relaunch each
    /// reader once. A 2026-10-22 VM run of a dev build applied no Mac setting at all.
    /// Negative control: `?` on an item's snapshot in `Workstation::preference_changes`
    /// refuses the whole pass.
    #[test]
    fn a_dock_that_cannot_pin_aterm_leaves_the_other_desktop_rows_to_apply() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_with(&who, Desk::before(), false);
        let mut first = Workstation::of(desktop_items(&who));
        let snapshot = first.preference_changes(&sys).unwrap();
        assert!(
            snapshot.iter().all(|(target, _)| target.domain != DOCK),
            "{snapshot:?}"
        );
        let run = first.run_recording(&sys, Mode::Apply).unwrap();
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Unknown),
            ],
            "{:?}",
            run.rows
        );
        assert_eq!(
            run.rows[4].why.as_deref(),
            Some(
                "aterm is not installed at /Applications/aterm.app or \
                 /Users//alice/Applications/aterm.app; Dock left untouched"
            ),
            "{:?}",
            run.rows[4]
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], true),
                ("init.desktop@alice", true)
            ]
        );
        assert!(!crate::init_activation::pending(&sys, &who, "dock").unwrap());
        let desk = desk.lock().unwrap();
        assert!(
            !desk
                .events
                .iter()
                .any(|e| e.starts_with(&format!("write {DOCK}"))),
            "{:?}",
            desk.events
        );
        // The person's own pins, which the relaunched Dock gave GUIDs as it loaded them.
        let Some(PValue::Array(pins)) = desk.read(DOCK, "persistent-apps") else {
            panic!("{:?}", desk.read(DOCK, "persistent-apps"));
        };
        let labels: Vec<&PValue> = pins
            .iter()
            .filter_map(|pin| field(field(pin, "tile-data")?, "file-label"))
            .collect();
        assert_eq!(
            labels,
            [&PValue::Str("Mail".into()), &PValue::Str("Music".into())]
        );
        assert_eq!(
            desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
            Some(&PValue::Bool(true))
        );
        // The Dock process still relaunches once: it reads the widget and desktop keys
        // (WindowManager runs in it), not because the Dock row wrote anything.
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
    }

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
            &dock_apps(&usual_dock_apps())
        ));
        let ids: Vec<String> = journaled(&sys).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            [
                WIDGET_IDS[0],
                WIDGET_IDS[1],
                WIDGET_IDS[2],
                "init.desktop@alice",
                "init.dock@alice",
                "init.relaunch@alice"
            ]
        );
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        drop(desk);

        let (sys, desk) = desktop_of(&who, Desk::before());
        let pins = Target::user(DOCK, "persistent-apps", &who);
        defaults::relaunch(&sys, &who, "Dock").unwrap();
        sys.run(&pins.write_cmd(&dock_apps(&usual_dock_apps())))
            .unwrap();
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
    /// written read back changed.
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
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ]
        );
        assert_eq!(
            run.rows[4].why.as_deref(),
            Some("applied, but it still reads com.apple.dock persistent-apps")
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], true),
                ("init.desktop@alice", true),
                ("init.dock@alice", false)
            ]
        );
        let mut slow = Desk::before();
        slow.managed = Some(PValue::Array(vec![pin("Mail")]));
        slow.start = 3;
        let (sys, desk) = desktop_of(&who, slow);
        let run = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&run),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:#?}",
            desk.lock().unwrap().events
        );
        assert!(desk.lock().unwrap().starting().is_empty());
    }

    /// A pass that ends after its writes and before the relaunch (the process quit, the
    /// Mac slept) leaves every family's marker.
    #[test]
    fn a_pass_that_ends_between_its_writes_and_the_relaunch_relaunches_on_the_next() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let c = ctx(std::slice::from_ref(&who));
        for item in desktop_items(&who) {
            item.apply(&sys, &c).unwrap();
        }
        for family in [WIDGETS, "desktop", "dock"] {
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
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
        }
    }

    /// A relaunch that cannot be made (the GUI session lookup for the Dock failed) fails
    /// every row the Dock reads, here all but Notification Center's widgets, and keeps the
    /// written preferences and their markers, while the readers that could relaunch were
    /// (each once) and were waited on, and the marker those alone discharge (Notification
    /// Center's) is cleared, its row changed: a plan still plans the others, and the next
    /// pass relaunches the Dock and the Finder once each, without writing again, and not
    /// Notification Center, which already read its write.
    #[test]
    fn a_relaunch_that_fails_keeps_the_writes_pending_for_the_next_pass() {
        let who = user("alice", 501);
        let mut failing = Desk::before();
        failing.session_failures = 1;
        let (sys, desk) = desktop_of(&who, failing);
        let first = pass(&sys, desktop_items(&who), Mode::Apply);
        let but_center = |center: Outcome, rest: Outcome| {
            DESKTOP_IDS.map(|id| (id, if id == NC_ID { center } else { rest }))
        };
        assert_eq!(
            outcomes(&first),
            but_center(Outcome::Changed, Outcome::Failed)
        );
        for row in first.rows.iter().filter(|row| row.id != NC_ID) {
            let why = row.why.as_deref().unwrap();
            assert!(
                why.starts_with("applied, but not activated: /bin/launchctl print gui/501"),
                "{why}"
            );
        }
        for family in [WIDGETS, "desktop", "dock"] {
            assert!(crate::init_activation::pending(&sys, &who, family).unwrap());
        }
        assert!(
            !crate::init_activation::pending(&sys, &who, crate::init_activation::CONTAINED_MARKER)
                .unwrap()
        );
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
            but_center(Outcome::Ok, Outcome::Planned)
        );
        let recovered = pass(&sys, desktop_items(&who), Mode::Apply);
        assert_eq!(
            outcomes(&recovered),
            but_center(Outcome::Ok, Outcome::Changed)
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
            (1, 1, 0)
        );
    }

    /// Each row settles alone: a reader that does not start again (Notification Center,
    /// here never started again by launchd) fails the rows whose keys it reads and keeps
    /// the marker of those keys (Notification Center's own), while the rows all of whose
    /// readers are up are read back, verified, and cleared, as is the widgets' marker of the
    /// Window Manager's keys, whose Dock is up; the doctor then rolls back only Notification
    /// Center's widgets, and leaves the changes that took.
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
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Failed),
                ("init.desktop@alice", Outcome::Changed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        assert_eq!(
            run.rows[2].why.as_deref(),
            Some(
                "applied, but not activated: NotificationCenter has not started again a \
                 minute after its relaunch; activation remains pending"
            )
        );
        assert_eq!(
            verified(&run),
            [
                (WIDGET_IDS[0], true),
                (WIDGET_IDS[1], true),
                (WIDGET_IDS[2], false),
                ("init.desktop@alice", true),
                ("init.dock@alice", true)
            ]
        );
        for (family, pending) in [
            (WIDGETS, false),
            (crate::init_activation::CONTAINED_MARKER, true),
            ("desktop", false),
            ("dock", false),
        ] {
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
            &dock_apps(&usual_dock_apps())
        ));
        assert_eq!(desk.count("kill Dock"), 1, "{:?}", desk.events);
    }

    /// A relaunch that cannot be made (Finder's `killall` fails, signalling nothing) fails
    /// the family it reads, and the wait for the readers already relaunched goes on: the
    /// Dock, slow to start, is past its start before the pass ends, so the doctor's
    /// rollback of the desktop keys and its relaunch of their readers kill a Dock that is
    /// up, which writes nothing back.
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
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.desktop@alice", Outcome::Failed),
                ("init.dock@alice", Outcome::Changed),
            ],
            "{:?}",
            run.rows
        );
        let why = run.rows[3].why.as_deref().unwrap();
        assert!(
            why.starts_with("applied, but not activated: ")
                && why.contains("killall -u alice Finder"),
            "{why}"
        );
        assert!(crate::init_activation::pending(&sys, &who, "desktop").unwrap());
        assert_eq!(desk.lock().unwrap().starting(), []);
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
            &dock_apps(&usual_dock_apps())
        ));
    }

    /// A family is owed its relaunch from its first write: one whose write fails part way
    /// has still made the writes before it, so its readers relaunch after them with the
    /// pass's other families, its marker is cleared, and the relaunch's line names it.
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
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (WIDGET_IDS[2], Outcome::Changed),
                ("init.dock@alice", Outcome::Failed),
            ],
            "{:?}",
            run.rows
        );
        assert!(
            run.rows[3]
                .why
                .as_deref()
                .is_some_and(|why| why.contains("static-apps")),
            "{:?}",
            run.rows[3]
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

    /// Whether `call` reads, writes, censuses or relaunches Notification Center or its
    /// preferences.
    fn touches_notification_center(call: &str) -> bool {
        call.contains(NOTIFICATION_CENTER) || call.contains("NotificationCenter")
    }

    /// A pass whose process holds no known Full Disk Access (refused, or not measured)
    /// leaves Notification Center's container alone on every path: the snapshot, the
    /// probe, the apply, the relaunch and its look, the plan and the read-back issue no
    /// command that names it, and its widgets stay as they were.
    #[test]
    fn without_full_disk_access_a_pass_never_touches_notification_center_and_settles() {
        let who = user("alice", 501);
        for unknown in [None, Some(false)] {
            let (sys, desk) = desktop_of(&who, Desk::before());
            let mut first = Workstation::of(desktop_items_at(&who, unknown));
            let snapshot = first.preference_changes(&sys).unwrap();
            assert!(
                snapshot
                    .iter()
                    .all(|(target, _)| target.domain != NOTIFICATION_CENTER),
                "{snapshot:?}"
            );
            let run = first.run_recording(&sys, Mode::Apply).unwrap();
            // Notification Center's row reaches no key of its own, so it reads as wanted
            // and is withheld; the Window Manager's widget rows change.
            assert_eq!(
                outcomes(&run),
                DESKTOP_IDS.map(|id| (
                    id,
                    if id == NC_ID {
                        Outcome::Ok
                    } else {
                        Outcome::Changed
                    }
                )),
                "{:?}",
                run.rows
            );
            assert_eq!(run.withheld, [NC_ID]);
            let widgets = &run.rows[2];
            assert_eq!(
                widgets.title,
                "No Notification Center widgets; they wait for Full Disk Access"
            );
            assert!(
                widgets
                    .to
                    .as_deref()
                    .is_some_and(|to| to.contains("com.apple.notificationcenterui")),
                "{widgets:?}"
            );
            let calls = sys.calls();
            assert!(
                !calls.iter().any(|call| touches_notification_center(call)),
                "{calls:#?}"
            );
            {
                let desk = desk.lock().unwrap();
                assert_eq!(
                    (
                        desk.count("kill Dock"),
                        desk.count("kill Finder"),
                        desk.count("kill NotificationCenter")
                    ),
                    (1, 1, 0),
                    "{:?}",
                    desk.events
                );
                assert_eq!(
                    desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
                    Some(&PValue::Bool(true))
                );
                let PValue::Dict(widgets) = desk.read(NOTIFICATION_CENTER, "widgets").unwrap()
                else {
                    unreachable!()
                };
                assert!(
                    widgets.iter().any(|(key, value)| key == "instances"
                        && *value == PValue::Array(vec![PValue::Data(vec![42])])),
                    "{widgets:?}"
                );
            }
            for family in [WIDGETS, "desktop", "dock"] {
                assert!(!crate::init_activation::pending(&sys, &who, family).unwrap());
            }
            let lines = journaled(&sys).len();
            let (events, calls) = (desk.lock().unwrap().events.len(), sys.calls().len());
            for mode in [Mode::Plan, Mode::Apply] {
                let again = pass(&sys, desktop_items_at(&who, unknown), mode);
                assert_eq!(
                    outcomes(&again),
                    DESKTOP_IDS.map(|id| (id, Outcome::Ok)),
                    "{mode:?}: {:?}",
                    again.rows
                );
            }
            assert_eq!(
                desk.lock().unwrap().events.len(),
                events,
                "{:?}",
                desk.lock().unwrap().events
            );
            assert_eq!(journaled(&sys).len(), lines);
            let later = &sys.calls()[calls..];
            assert!(
                !later.iter().any(|call| touches_notification_center(call)
                    || call.contains("killall")
                    || call.contains(" write ")),
                "{later:#?}"
            );
        }

        let (sys, desk) = desktop_of(&who, Desk::before());
        let run = pass(&sys, desktop_items_at(&who, Some(true)), Mode::Apply);
        assert_eq!(
            run.rows
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>()[..3],
            [
                "No desktop widgets",
                "No Stage Manager widgets",
                "No Notification Center widgets"
            ]
        );
        assert!(run.withheld.is_empty());
        assert!(
            sys.calls()
                .iter()
                .any(|call| touches_notification_center(call))
        );
        assert!(sys.calls().contains(
            &"alice$ /usr/bin/defaults export com.apple.notificationcenterui -".to_owned()
        ));
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 1);
    }

    /// Measured (macOS 27.0 VM, 2026-10-22): a process without Full Disk Access reads
    /// Notification Center's container as an empty domain.
    #[test]
    fn a_pass_that_reaches_a_container_it_cannot_read_never_settles_and_one_that_waits_does() {
        let who = user("alice", 501);
        let mut hidden = Desk::before();
        hidden.denied = Some(NOTIFICATION_CENTER);
        let (sys, desk) = desktop_of(&who, hidden);
        for first in [true, false] {
            let run = pass(
                &sys,
                selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
                Mode::Apply,
            );
            let window_manager = if first { Outcome::Changed } else { Outcome::Ok };
            assert_eq!(
                outcomes(&run),
                [
                    (WIDGET_IDS[0], window_manager),
                    (WIDGET_IDS[1], window_manager),
                    (NC_ID, Outcome::Failed)
                ]
            );
            assert_eq!(
                run.rows[2].why.as_deref(),
                Some("applied, but it still reads com.apple.notificationcenterui widgets")
            );
        }
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 2);
        assert_eq!(
            desk.lock()
                .unwrap()
                .count("write com.apple.notificationcenterui widgets"),
            2
        );

        let mut hidden = Desk::before();
        hidden.denied = Some(NOTIFICATION_CENTER);
        let (sys, desk) = desktop_of(&who, hidden);
        let widgets = || selected_desktop_items_at(&who, None, &[Setting::Widgets]);
        let first = pass(&sys, widgets(), Mode::Apply);
        assert_eq!(
            outcomes(&first),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        let second = pass(&sys, widgets(), Mode::Apply);
        assert_eq!(outcomes(&second), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        let desk = desk.lock().unwrap();
        assert_eq!(
            (
                desk.count("kill Dock"),
                desk.count("kill NotificationCenter")
            ),
            (1, 0),
            "{:?}",
            desk.events
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| touches_notification_center(call))
        );
    }

    /// The follow-up items of `settings` ([`withheld_items`]), run by a process whose Full
    /// Disk Access is `full_disk_access`.
    fn follow_up_items(
        who: &User,
        full_disk_access: Option<bool>,
        settings: &[Setting],
    ) -> Vec<Box<dyn Item>> {
        let mut selected = Selection::none();
        for &setting in settings {
            selected.set(setting, true);
        }
        let mut context = ctx(std::slice::from_ref(who));
        context.facts.full_disk_access = full_disk_access;
        withheld_items(&context, selected).unwrap()
    }

    /// The whole sequence, item by item: a pass without Full Disk Access hides the desktop
    /// and Stage Manager widgets, leaves Notification Center alone and says so (its run
    /// names the row withheld); the person then turns the desktop widgets back on.
    #[test]
    fn a_follow_up_with_the_grant_clears_notification_center_alone_and_once() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let first = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&first),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        assert_eq!(first.withheld, [NC_ID]);
        desk.lock()
            .unwrap()
            .write(WINDOW_MANAGER, "StandardHideWidgets", PValue::Bool(false));

        let calls = sys.calls().len();
        let waiting = pass(
            &sys,
            follow_up_items(&who, None, &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&waiting), [(NC_ID, Outcome::Ok)]);
        assert_eq!(waiting.withheld, [NC_ID]);
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| touches_notification_center(call) || call.contains(" write ")),
            "{:#?}",
            &sys.calls()[calls..]
        );

        let calls = sys.calls().len();
        let kills = desk.lock().unwrap().count("kill Dock");
        let follow_up = pass(
            &sys,
            follow_up_items(&who, Some(true), &[Setting::Widgets, Setting::Dock]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&follow_up),
            [(NC_ID, Outcome::Changed)],
            "{:?}",
            follow_up.rows
        );
        assert_eq!(follow_up.rows[0].title, "No Notification Center widgets");
        assert!(follow_up.withheld.is_empty());
        {
            let desk = desk.lock().unwrap();
            let PValue::Dict(widgets) = desk.read(NOTIFICATION_CENTER, "widgets").unwrap() else {
                unreachable!()
            };
            assert!(
                widgets
                    .iter()
                    .any(|(key, value)| key == "instances" && *value == PValue::Array(Vec::new())),
                "{widgets:?}"
            );
            assert_eq!(
                desk.read(WINDOW_MANAGER, "StandardHideWidgets"),
                Some(&PValue::Bool(false)),
                "the person's change stays"
            );
            assert_eq!(
                desk.count("kill NotificationCenter"),
                1,
                "{:?}",
                desk.events
            );
            assert_eq!(desk.count("kill Dock"), kills, "{:?}", desk.events);
        }
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| call.contains(WINDOW_MANAGER) || call.contains(DOCK)),
            "{:#?}",
            &sys.calls()[calls..]
        );
        for marker in [WIDGETS, crate::init_activation::CONTAINED_MARKER] {
            assert!(!crate::init_activation::pending(&sys, &who, marker).unwrap());
        }

        let events = desk.lock().unwrap().events.len();
        let again = pass(
            &sys,
            follow_up_items(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&again), [(NC_ID, Outcome::Ok)]);
        assert_eq!(desk.lock().unwrap().events.len(), events);

        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Plan,
        );
        assert!(granted.withheld.is_empty());
    }

    /// Notification Center's own marker: a pass with the grant that writes its widgets and
    /// ends before the relaunch leaves it beside the widgets' marker.
    #[test]
    fn a_pass_without_the_grant_never_clears_what_notification_center_still_owes() {
        let who = user("alice", 501);
        let (sys, desk) = desktop_of(&who, Desk::before());
        let mut c = ctx(std::slice::from_ref(&who));
        c.facts.full_disk_access = Some(true);
        for item in selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]) {
            item.apply(&sys, &c).unwrap();
        }
        let owed = |marker: &str| crate::init_activation::pending(&sys, &who, marker).unwrap();
        assert!(owed(WIDGETS) && owed(crate::init_activation::CONTAINED_MARKER));
        assert_eq!(
            desk.lock()
                .unwrap()
                .count("write com.apple.notificationcenterui widgets"),
            1
        );

        let calls = sys.calls().len();
        let waiting = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&waiting),
            [
                (WIDGET_IDS[0], Outcome::Changed),
                (WIDGET_IDS[1], Outcome::Changed),
                (NC_ID, Outcome::Ok)
            ]
        );
        let quiet = pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&quiet), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        assert!(!owed(WIDGETS));
        assert!(owed(crate::init_activation::CONTAINED_MARKER));
        assert!(
            !sys.calls()[calls..]
                .iter()
                .any(|call| touches_notification_center(call)
                    || call.contains(crate::init_activation::CONTAINED_MARKER)),
            "{:#?}",
            &sys.calls()[calls..]
        );
        {
            let desk = desk.lock().unwrap();
            assert_eq!(
                (
                    desk.count("kill Dock"),
                    desk.count("kill NotificationCenter")
                ),
                (1, 0),
                "{:?}",
                desk.events
            );
        }

        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(
            outcomes(&granted),
            [
                (WIDGET_IDS[0], Outcome::Ok),
                (WIDGET_IDS[1], Outcome::Ok),
                (NC_ID, Outcome::Changed)
            ]
        );
        assert!(!owed(crate::init_activation::CONTAINED_MARKER));
        {
            let desk = desk.lock().unwrap();
            assert_eq!(
                (
                    desk.count("kill Dock"),
                    desk.count("kill NotificationCenter"),
                    desk.count("write com.apple.notificationcenterui widgets")
                ),
                (1, 1, 1),
                "{:?}",
                desk.events
            );
        }

        let mut cleared = Desk::before();
        cleared.write(
            NOTIFICATION_CENTER,
            "widgets",
            PValue::Dict(vec![
                ("vers".into(), PValue::Int(1)),
                ("instances".into(), PValue::Array(Vec::new())),
            ]),
        );
        let (sys, desk) = desktop_of(&who, cleared);
        for item in selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]) {
            item.apply(&sys, &c).unwrap();
        }
        assert!(
            !crate::init_activation::pending(&sys, &who, crate::init_activation::CONTAINED_MARKER)
                .unwrap()
        );
        pass(
            &sys,
            selected_desktop_items_at(&who, None, &[Setting::Widgets]),
            Mode::Apply,
        );
        let granted = pass(
            &sys,
            selected_desktop_items_at(&who, Some(true), &[Setting::Widgets]),
            Mode::Apply,
        );
        assert_eq!(outcomes(&granted), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
        assert_eq!(desk.lock().unwrap().count("kill NotificationCenter"), 0);
    }

    /// An older aterm wrote one marker (`widgets.pending`) before every widgets write,
    /// Notification Center's widget list with the Window Manager's keys.
    #[test]
    fn an_older_aterms_widgets_marker_relaunches_notification_center_once_with_the_grant() {
        let who = user("alice", 501);
        let written = || {
            let mut desk = Desk::before();
            for key in ["StandardHideWidgets", "StageManagerHideWidgets"] {
                desk.write(WINDOW_MANAGER, key, PValue::Bool(true));
            }
            desk.write(
                NOTIFICATION_CENTER,
                "widgets",
                PValue::Dict(vec![
                    ("vers".into(), PValue::Int(1)),
                    ("instances".into(), PValue::Array(Vec::new())),
                ]),
            );
            desk
        };
        let kills = |desk: &Arc<Mutex<Desk>>| {
            let desk = desk.lock().unwrap();
            (
                desk.count("kill Dock"),
                desk.count("kill NotificationCenter"),
                desk.events
                    .iter()
                    .filter(|e| e.starts_with("write "))
                    .count(),
            )
        };
        let widgets =
            |granted: Option<bool>| selected_desktop_items_at(&who, granted, &[Setting::Widgets]);
        for marker in [crate::init_activation::LEGACY_WIDGETS_MARKER, WIDGETS] {
            let legacy = marker == crate::init_activation::LEGACY_WIDGETS_MARKER;
            for granted in [Some(true), None] {
                let (sys, desk) = desktop_of(&who, written());
                crate::init_activation::begin(&sys, &who, marker).unwrap();
                let owed =
                    |marker: &str| crate::init_activation::pending(&sys, &who, marker).unwrap();
                let calls = sys.calls().len();
                let first = pass(&sys, widgets(granted), Mode::Apply);
                assert!(
                    granted.is_some()
                        || !sys.calls()[calls..]
                            .iter()
                            .any(|call| touches_notification_center(call)),
                    "{:#?}",
                    &sys.calls()[calls..]
                );
                // The Window Manager's rows carry the marker; Notification Center's row
                // relaunches its reader only for an older aterm's marker, with the grant.
                let center = if legacy && granted.is_some() {
                    Outcome::Changed
                } else {
                    Outcome::Ok
                };
                assert_eq!(
                    outcomes(&first),
                    [
                        (WIDGET_IDS[0], Outcome::Changed),
                        (WIDGET_IDS[1], Outcome::Changed),
                        (NC_ID, center)
                    ],
                    "{marker} {granted:?}: {:?}",
                    first.rows
                );
                let nc_owed = legacy && granted.is_none();
                assert_eq!(
                    kills(&desk),
                    (1, usize::from(legacy && granted.is_some()), 0),
                    "{marker} {granted:?}: {:?}",
                    desk.lock().unwrap().events
                );
                for marker in [crate::init_activation::LEGACY_WIDGETS_MARKER, WIDGETS] {
                    assert!(!owed(marker), "{marker} {granted:?}");
                }
                assert_eq!(
                    owed(crate::init_activation::CONTAINED_MARKER),
                    nc_owed,
                    "{marker} {granted:?}"
                );
                let again = pass(&sys, widgets(Some(true)), Mode::Apply);
                assert_eq!(
                    outcomes(&again),
                    [
                        (WIDGET_IDS[0], Outcome::Ok),
                        (WIDGET_IDS[1], Outcome::Ok),
                        (
                            NC_ID,
                            if nc_owed {
                                Outcome::Changed
                            } else {
                                Outcome::Ok
                            }
                        )
                    ],
                    "{marker} {granted:?}"
                );
                let quiet = pass(&sys, widgets(Some(true)), Mode::Apply);
                assert_eq!(outcomes(&quiet), WIDGET_IDS.map(|id| (id, Outcome::Ok)));
                assert!(!owed(crate::init_activation::CONTAINED_MARKER));
                assert_eq!(
                    kills(&desk),
                    (1, usize::from(legacy), 0),
                    "{marker} {granted:?}: {:?}",
                    desk.lock().unwrap().events
                );
            }
        }
    }
}
